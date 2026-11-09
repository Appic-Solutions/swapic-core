//! `release` (tag 4), design 4.8: as accepting as safety allows, because a message whose
//! `destinationCaller` is a Swapic `auth` can be received by nothing else and CCTP has no refund. It
//! is never stopped by a pause.

use super::complete::{check_header, check_receive_pins};
use crate::accounts::{
    is, is_program_address, key_of, now, require, require_top_level, shard_of, token_amount,
    ATA_ADDRESS, TOKEN_ADDRESS,
};
use crate::cctp::{receive_message, token_transfer};
use crate::consts;
use crate::fail;
use crate::log;
use crate::state_check::{auth_clean, token_account_clean};
use pinocchio::cpi::{Seed, Signer};
use pinocchio::{AccountView, Address, ProgramResult};
use swapic_solana_wire::args::ArrivalArgs;
use swapic_solana_wire::cctp::Message;
use swapic_solana_wire::consts as wire;
use swapic_solana_wire::envelope::read_release_terms;
use swapic_solana_wire::errors::SwapicError;

const SUBMITTER: usize = 0;
const CONFIG: usize = 1;
const CALLER: usize = 2;
const VAULT: usize = 3;
const OUTPUT: usize = 4;
const VAULT_AUTH: usize = 5;
const HEAD: usize = 22;

pub fn process(accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    require_top_level()?;
    if accounts.len() != HEAD {
        return Err(fail(SwapicError::ShortData));
    }
    let a: &[AccountView] = accounts;
    let args = ArrivalArgs::decode(data).map_err(fail)?;
    require(&a[CONFIG], &consts::CONFIG, SwapicError::WrongConfig)?;
    check_receive_pins(a)?;

    let message = Message::new(args.message).map_err(fail)?;
    check_header(&message)?;
    let caller = shard_of(message.destination_caller(), &consts::AUTH)
        .ok_or(fail(SwapicError::NotForThisAuthOrVault))?;
    let vault = shard_of(message.mint_recipient(), &consts::VAULT)
        .ok_or(fail(SwapicError::NotForThisAuthOrVault))?;
    require(&a[CALLER], &consts::AUTH[caller], SwapicError::WrongAuth)?;
    require(&a[VAULT], &consts::VAULT[vault], SwapicError::WrongVault)?;
    require(&a[VAULT_AUTH], &consts::AUTH[vault], SwapicError::WrongAuth)?;

    if !a[SUBMITTER].is_signer() {
        return Err(fail(SwapicError::NotCanisterOrRecipient));
    }
    let submitter = key_of(&a[SUBMITTER]);
    let terms = read_release_terms(message.hook());
    let (ref_bytes, misrouted) = match terms {
        Some(terms) if !is_program_address(&terms.recipient) => {
            let (ata, _) = Address::find_program_address(
                &[&terms.recipient, &wire::TOKEN_PROGRAM, &wire::USDC_MINT],
                &ATA_ADDRESS,
            );
            if !is(&a[OUTPUT], ata.as_array()) {
                return Err(fail(SwapicError::BadOutputAccount));
            }
            // the recipient's own USDC account, initialized (a create may come before this instruction)
            if !a[OUTPUT].owned_by(&TOKEN_ADDRESS) {
                return Err(fail(SwapicError::BadOutputAccount));
            }
            if now()? <= terms.deadline as i64
                && submitter != &consts::CANISTER
                && submitter != &terms.recipient
            {
                return Err(fail(SwapicError::NotCanisterOrRecipient));
            }
            (terms.ref_bytes, false)
        }
        _ => {
            // unreadable, or a recipient that is one of the program's own: only the canister, only
            // to Swapic's fee account, for a refund by hand
            if submitter != &consts::CANISTER {
                return Err(fail(SwapicError::NotCanisterOrRecipient));
            }
            require(
                &a[OUTPUT],
                &consts::FEE_ACCOUNT,
                SwapicError::WrongFeeAccount,
            )?;
            (terms.map(|t| t.ref_bytes).unwrap_or([0u8; 32]), true)
        }
    };

    let caller_lamports = a[CALLER].lamports();
    let vault_auth_lamports = a[VAULT_AUTH].lamports();
    let vault_before = token_amount(&a[VAULT])?;

    let caller_index = [caller as u8];
    let caller_bump = [consts::AUTH_BUMP[caller]];
    let caller_seeds = [
        Seed::from(wire::SEED_AUTH),
        Seed::from(&caller_index),
        Seed::from(&caller_bump),
    ];
    receive_message(
        a,
        &a[CALLER],
        &a[VAULT],
        args.message,
        args.attestation,
        &[Signer::from(&caller_seeds)],
    )?;

    let minted = token_amount(&a[VAULT])?
        .checked_sub(vault_before)
        .ok_or(fail(SwapicError::NothingMinted))?;
    if minted == 0 {
        return Err(fail(SwapicError::NothingMinted));
    }

    let vault_index = [vault as u8];
    let vault_bump = [consts::AUTH_BUMP[vault]];
    let vault_seeds = [
        Seed::from(wire::SEED_AUTH),
        Seed::from(&vault_index),
        Seed::from(&vault_bump),
    ];
    token_transfer(
        &a[VAULT],
        &a[OUTPUT],
        &a[VAULT_AUTH],
        minted,
        &[Signer::from(&vault_seeds)],
    )?;

    if token_amount(&a[VAULT])? != vault_before {
        return Err(fail(SwapicError::VaultNotAtBaseline));
    }
    token_account_clean(
        &a[VAULT],
        &wire::USDC_MINT,
        &consts::AUTH[vault],
        SwapicError::VaultStateChanged,
    )?;
    auth_clean(&a[CALLER], caller_lamports)?;
    auth_clean(&a[VAULT_AUTH], vault_auth_lamports)?;

    log::released(&ref_bytes, minted, misrouted);
    Ok(())
}
