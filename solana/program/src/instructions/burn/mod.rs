//! `swap_burn` (tag 1) and `burn` (tag 2): out of Solana, design 4.6.

use crate::accounts::{
    is, key_of, now, read_config, require, require_top_level, shard_of, token_amount,
};
use crate::cctp::{deposit_for_burn, system_transfer, token_transfer, BurnAccounts};
use crate::consts;
use crate::fail;
use crate::jupiter::{self, Way};
use crate::log;
use crate::state_check::{auth_clean, token_account_clean};
use pinocchio::cpi::{Seed, Signer};
use pinocchio::error::ProgramError;
use pinocchio::sysvars::rent::Rent;
use pinocchio::sysvars::Sysvar;
use pinocchio::{AccountView, Address, ProgramResult};
use swapic_solana_wire::args::{BurnArgs, Payload, RefSource, TAG_SWAP_BURN};
use swapic_solana_wire::cctp::{max_fee_of, BurnData, FINALITY_FAST, FINALITY_STANDARD};
use swapic_solana_wire::consts as wire;
use swapic_solana_wire::envelope::Action;
use swapic_solana_wire::errors::SwapicError;

/// `keccak256(memo)` by syscall: the ref of a burn whose terms ride on chain (decision 3).
#[inline(always)]
fn keccak(memo: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    #[cfg(target_os = "solana")]
    {
        let parts = [memo];
        // SAFETY: the syscall reads a slice of slices and writes 32 bytes
        unsafe {
            pinocchio::syscalls::sol_keccak256(
                parts.as_ptr() as *const u8,
                parts.len() as u64,
                out.as_mut_ptr(),
            );
        }
    }
    #[cfg(not(target_os = "solana"))]
    let _ = memo;
    out
}

#[inline(always)]
const fn event_len() -> usize {
    if cfg!(feature = "test-short-rent") {
        wire::EVENT_ACCOUNT_LEN as usize - 1
    } else {
        wire::EVENT_ACCOUNT_LEN as usize
    }
}

/// The account positions of design 4.6.
const USER: usize = 0;
const CONFIG: usize = 1;
const CANISTER: usize = 2;
const AUTH: usize = 3;
const VAULT: usize = 4;
const FEE_ACCOUNT: usize = 5;
const RENT: usize = 6;
const EVENT: usize = 7;
const SENDER_AUTHORITY: usize = 8;
const DENYLIST: usize = 9;
const MESSAGE_TRANSMITTER: usize = 10;
const TOKEN_MESSENGER: usize = 11;
const REMOTE_TOKEN_MESSENGER: usize = 12;
const TOKEN_MINTER: usize = 13;
const LOCAL_TOKEN: usize = 14;
const USDC: usize = 15;
const MT: usize = 16;
const TMM: usize = 17;
const TOKEN: usize = 18;
const SYSTEM: usize = 19;
const EVENT_AUTHORITY: usize = 20;
/// `swap_burn`: Jupiter. `burn`: the user's USDC account.
const TAIL: usize = 21;
const ROUTE: usize = 22;

pub fn process(accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    require_top_level()?;
    if accounts.len() < ROUTE {
        return Err(fail(SwapicError::ShortData));
    }
    let a: &[AccountView] = accounts;
    let args = BurnArgs::decode(data).map_err(fail)?;
    let config = read_config(&a[CONFIG], true)?;

    // the pair of accounts this burn runs through
    let shard = shard_of(key_of(&a[AUTH]), &consts::AUTH).ok_or(fail(SwapicError::WrongAuth))?;
    require(&a[VAULT], &consts::VAULT[shard], SwapicError::WrongVault)?;
    check_pins(a, shard)?;

    // the arguments only this program can rule on
    let allowed = match args.payload.action() {
        Action::CkDeposit => config.allows_ck_to(args.domain),
        _ => config.allows_swap_to(args.domain),
    };
    if !allowed {
        return Err(fail(SwapicError::DomainNotAllowed));
    }
    let clock = now()?;
    let deadline = args.deadline as i64;
    if deadline < clock.saturating_add(config.min_deadline_s as i64)
        || deadline > clock.saturating_add(config.max_deadline_s as i64)
    {
        return Err(fail(SwapicError::DeadlineOutOfWindow));
    }
    let mut receiver_shaped = [0u8; 32];
    receiver_shaped[12..].copy_from_slice(args.recipient);
    if receiver_shaped == consts::CCTP_RECEIVER
        || *args.recipient == consts::SWAPIC_EVM
        || *args.recipient == consts::RUNNER_EVM
    {
        return Err(fail(SwapicError::BadRecipient));
    }

    let ref_bytes = match args.ref_source {
        RefSource::Direct(r) => *r,
        RefSource::Memo(memo) => keccak(memo),
    };

    // the user or, with the canister's signature, the float pays CCTP's event account
    if !a[USER].is_signer() {
        return Err(fail(SwapicError::WrongKey));
    }
    let canister_signed = a[CANISTER].is_signer();
    if args.float != canister_signed {
        return Err(fail(SwapicError::FloatWithoutCanister));
    }
    let rent_before = a[RENT].lamports();
    if !args.float {
        let rent_due = Rent::get()?.try_minimum_balance(event_len())?;
        system_transfer(&a[USER], &a[RENT], rent_due, &[])?;
    }

    // one event account per live ref, or (the fallback of 4.12) a fresh keypair that signs
    let event_bump = check_event(a, &args, config.event_keypair_allowed, &ref_bytes)?;

    let auth_lamports = a[AUTH].lamports();
    let vault_before = token_amount(&a[VAULT])?;

    // into the vault: a route the user signs, or the user's own USDC
    if args.tag == TAG_SWAP_BURN {
        require(&a[TAIL], &wire::JUPITER, SwapicError::WrongProgram)?;
        let route = &a[ROUTE..];
        jupiter::check_route(
            route,
            Way::Out {
                shard,
                event: key_of(&a[EVENT]),
            },
        )?;
        jupiter::call(route, args.jupiter, key_of(&a[USER]), &[])?;
    } else {
        if a.len() != ROUTE {
            return Err(fail(SwapicError::ShortData));
        }
        token_transfer(&a[TAIL], &a[VAULT], &a[USER], args.amount, &[])?;
    }

    let produced = token_amount(&a[VAULT])?
        .checked_sub(vault_before)
        .ok_or(fail(SwapicError::NothingProduced))?;
    if produced == 0 {
        return Err(fail(SwapicError::NothingProduced));
    }
    if args.tag == TAG_SWAP_BURN && produced < args.amount {
        return Err(fail(SwapicError::BelowMinUsdc));
    }
    let fee = args.fee as u64;
    if fee > produced {
        return Err(fail(SwapicError::FeeAboveProduced));
    }
    let amount = produced - fee;
    if amount < config.min_burn {
        return Err(fail(SwapicError::BelowMinBurn));
    }

    let index = [shard as u8];
    let auth_bump = [consts::AUTH_BUMP[shard]];
    let auth_seeds = [
        Seed::from(wire::SEED_AUTH),
        Seed::from(&index),
        Seed::from(&auth_bump),
    ];
    let auth_signer = Signer::from(&auth_seeds);
    if fee > 0 {
        token_transfer(
            &a[VAULT],
            &a[FEE_ACCOUNT],
            &a[AUTH],
            fee,
            core::slice::from_ref(&auth_signer),
        )?;
    }

    let max_fee = max_fee_of(amount, args.max_fee_deci_bps);
    if max_fee >= amount {
        return Err(fail(SwapicError::MaxFeeTooHigh));
    }

    // Swap and CkDeposit are received by `CctpReceiver`; a Deliver mints to its recipient, and anyone may relay it
    let mut mint_recipient = consts::CCTP_RECEIVER;
    let mut destination_caller = consts::CCTP_RECEIVER;
    if let Payload::Deliver = args.payload {
        mint_recipient = receiver_shaped;
        destination_caller = [0u8; 32];
    }
    let envelope = args.envelope(&ref_bytes).encode();
    let burn = BurnData {
        amount,
        domain: args.domain as u32,
        mint_recipient: &mint_recipient,
        destination_caller: &destination_caller,
        max_fee,
        min_finality: if args.fast {
            FINALITY_FAST
        } else {
            FINALITY_STANDARD
        },
        hook: &envelope,
    }
    .encode();

    let rent_seeds = [
        Seed::from(wire::SEED_RENT),
        Seed::from(core::slice::from_ref(&consts::RENT_BUMP)),
    ];
    let event_bump_seed = [event_bump.unwrap_or(0)];
    let event_seeds = [
        Seed::from(wire::SEED_EVENT),
        Seed::from(&ref_bytes),
        Seed::from(&event_bump_seed),
    ];
    let burn_accounts = BurnAccounts {
        owner: &a[AUTH],
        rent_payer: &a[RENT],
        sender_authority: &a[SENDER_AUTHORITY],
        burn_token_account: &a[VAULT],
        denylist: &a[DENYLIST],
        message_transmitter: &a[MESSAGE_TRANSMITTER],
        token_messenger: &a[TOKEN_MESSENGER],
        remote_token_messenger: &a[REMOTE_TOKEN_MESSENGER],
        token_minter: &a[TOKEN_MINTER],
        local_token: &a[LOCAL_TOKEN],
        mint: &a[USDC],
        event: &a[EVENT],
        mt_program: &a[MT],
        tmm_program: &a[TMM],
        token_program: &a[TOKEN],
        system_program: &a[SYSTEM],
        event_authority: &a[EVENT_AUTHORITY],
    };
    let signers = [
        auth_signer,
        Signer::from(&rent_seeds),
        Signer::from(&event_seeds),
    ];
    let signer_count = if event_bump.is_some() { 3 } else { 2 };
    deposit_for_burn(&burn_accounts, &burn, &signers[..signer_count])?;

    // spend all or revert: everything that arrived left as the fee and the burn
    if token_amount(&a[VAULT])? != vault_before {
        return Err(fail(SwapicError::VaultNotAtBaseline));
    }
    token_account_clean(
        &a[VAULT],
        &wire::USDC_MINT,
        &consts::AUTH[shard],
        SwapicError::VaultStateChanged,
    )?;
    auth_clean(&a[AUTH], auth_lamports)?;
    // a change in CCTP's event size can never draw on the float without the canister (attack L6)
    if !args.float && a[RENT].lamports() < rent_before {
        return Err(fail(SwapicError::FloatDrawn));
    }

    log::burned(
        &ref_bytes,
        args.domain,
        args.payload.action() as u8,
        produced,
        args.fee,
        amount,
    );
    Ok(())
}

/// Every fixed account CCTP does not validate with seeds is compared before any CPI (rule 3).
#[inline(never)]
fn check_pins(a: &[AccountView], shard: usize) -> ProgramResult {
    require(&a[CANISTER], &consts::CANISTER, SwapicError::WrongKey)?;
    require(
        &a[FEE_ACCOUNT],
        &consts::FEE_ACCOUNT,
        SwapicError::WrongFeeAccount,
    )?;
    require(&a[RENT], &consts::RENT, SwapicError::WrongRent)?;
    require(
        &a[SENDER_AUTHORITY],
        &wire::SENDER_AUTHORITY,
        SwapicError::WrongProgram,
    )?;
    require(
        &a[DENYLIST],
        &consts::DENYLIST[shard],
        SwapicError::WrongProgram,
    )?;
    require(
        &a[MESSAGE_TRANSMITTER],
        &wire::MESSAGE_TRANSMITTER,
        SwapicError::WrongProgram,
    )?;
    require(
        &a[TOKEN_MESSENGER],
        &wire::TOKEN_MESSENGER,
        SwapicError::WrongProgram,
    )?;
    require(
        &a[TOKEN_MINTER],
        &wire::TOKEN_MINTER,
        SwapicError::WrongProgram,
    )?;
    require(
        &a[LOCAL_TOKEN],
        &wire::LOCAL_TOKEN,
        SwapicError::WrongProgram,
    )?;
    require(&a[USDC], &wire::USDC_MINT, SwapicError::WrongProgram)?;
    require(&a[MT], &wire::MT, SwapicError::WrongProgram)?;
    require(&a[TMM], &wire::TMM, SwapicError::WrongProgram)?;
    require(&a[TOKEN], &wire::TOKEN_PROGRAM, SwapicError::WrongProgram)?;
    require(&a[SYSTEM], &wire::SYSTEM_PROGRAM, SwapicError::WrongProgram)?;
    require(
        &a[EVENT_AUTHORITY],
        &wire::TMM_EVENT_AUTHORITY,
        SwapicError::WrongProgram,
    )
}

/// The event account is the canonical PDA of the ref (answers its bump), or under the fallback a
/// keypair the transaction signed (answers `None`).
fn check_event(
    a: &[AccountView],
    args: &BurnArgs<'_>,
    keypair_allowed: bool,
    ref_bytes: &[u8; 32],
) -> Result<Option<u8>, ProgramError> {
    if args.event_keypair {
        if !keypair_allowed {
            return Err(fail(SwapicError::EventKeypairNotAllowed));
        }
        if !a[EVENT].is_signer() {
            return Err(fail(SwapicError::WrongEvent));
        }
        return Ok(None);
    }
    let (expected, bump) = Address::find_program_address(
        &[wire::SEED_EVENT, ref_bytes],
        &crate::accounts::PROGRAM_ADDRESS,
    );
    if !is(&a[EVENT], expected.as_array()) {
        return Err(fail(SwapicError::WrongEvent));
    }
    Ok(Some(bump))
}
