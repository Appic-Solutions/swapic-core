//! `complete` (tag 3): into Solana with a swap, design 4.7. The program receives the CCTP message
//! as `auth[i]`, swaps exactly the minted USDC to the hook's recipient, and refuses to end with a
//! dollar in the vault or less than `min_out` delivered.

use crate::accounts::{
    is, is_program_address, key_of, now, read_config, read_u64, require, require_top_level,
    shard_of, token_amount, TOKEN_2022_ADDRESS, TOKEN_ADDRESS,
};
use crate::cctp::receive_message;
use crate::consts;
use crate::fail;
use crate::jupiter::{self, Way};
use crate::log;
use crate::state_check::{auth_clean, token_account_clean};
use pinocchio::cpi::{Seed, Signer};
use pinocchio::error::ProgramError;
use pinocchio::{AccountView, ProgramResult};
use swapic_solana_wire::args::ArrivalArgs;
use swapic_solana_wire::cctp::Message;
use swapic_solana_wire::consts as wire;
use swapic_solana_wire::envelope::{SolanaEnvelope, SOLANA_ACTION_SWAP};
use swapic_solana_wire::errors::SwapicError;
use swapic_solana_wire::Pubkey;

const SUBMITTER: usize = 0;
const CONFIG: usize = 1;
const AUTH: usize = 2;
const VAULT: usize = 3;
const OUTPUT: usize = 4;
const WSOL: usize = 5;
const JUPITER: usize = 22;
const ROUTE: usize = 23;

/// The arrival head's fixed accounts CCTP does not validate with seeds. `release` uses them too.
pub(crate) fn check_receive_pins(a: &[AccountView]) -> ProgramResult {
    require(&a[7], &wire::MESSAGE_TRANSMITTER, SwapicError::WrongProgram)?;
    require(&a[9], &wire::TMM, SwapicError::WrongProgram)?;
    require(&a[10], &wire::SYSTEM_PROGRAM, SwapicError::WrongProgram)?;
    require(&a[12], &wire::MT, SwapicError::WrongProgram)?;
    require(&a[13], &wire::TOKEN_MESSENGER, SwapicError::WrongProgram)?;
    require(&a[15], &wire::TOKEN_MINTER, SwapicError::WrongProgram)?;
    require(&a[16], &wire::LOCAL_TOKEN, SwapicError::WrongProgram)?;
    require(&a[20], &wire::TOKEN_PROGRAM, SwapicError::WrongProgram)?;
    require(
        &a[21],
        &wire::TMM_EVENT_AUTHORITY,
        SwapicError::WrongProgram,
    )
}

/// The header rules `complete` and `release` share.
pub(crate) fn check_header(message: &Message<'_>) -> ProgramResult {
    if message.header_version() != 1
        || message.destination_domain() != wire::SOLANA_DOMAIN
        || message.recipient() != &wire::TMM
        || message.body_version() != 1
    {
        return Err(fail(SwapicError::BadHeader));
    }
    Ok(())
}

/// The output account: a token account of the recipient for the mint.
fn check_token_output(view: &AccountView, recipient: &Pubkey, mint: &Pubkey) -> ProgramResult {
    let classic = view.owned_by(&TOKEN_ADDRESS);
    if !classic && !view.owned_by(&TOKEN_2022_ADDRESS) {
        return Err(fail(SwapicError::BadOutputAccount));
    }
    let data = view.try_borrow()?;
    let shaped = data.len() == 165 || (!classic && data.len() > 165 && data[165] == 2);
    if !shaped || &data[..32] != mint || &data[32..64] != recipient || data[108] != 1 {
        return Err(fail(SwapicError::BadOutputAccount));
    }
    Ok(())
}

/// `wsol[i]` before the swap. It may be closed, when an earlier native arrival's route closed it and
/// no native arrival has recreated it since.
struct WsolBefore {
    open: bool,
    amount: u64,
    lamports: u64,
}

fn read_wsol(view: &AccountView) -> Result<WsolBefore, ProgramError> {
    let open = view.data_len() != 0;
    Ok(WsolBefore {
        open,
        amount: if open { token_amount(view)? } else { 0 },
        lamports: view.lamports(),
    })
}

/// True when the account was closed: no lamports, System-owned, no data.
fn is_closed(view: &AccountView) -> bool {
    view.lamports() == 0 && view.data_len() == 0 && view.owned_by(&crate::accounts::SYSTEM_ADDRESS)
}

/// `wsol[i]` after a CPI `auth[i]` signed: open and clean, at its baseline; or closed, which only a
/// native arrival may do. Answers whether the swap closed an account that was open.
fn check_wsol_after(
    view: &AccountView,
    before: &WsolBefore,
    native: bool,
    shard: usize,
) -> Result<bool, ProgramError> {
    if view.data_len() == 0 {
        if !before.open {
            return Ok(false);
        }
        if native && is_closed(view) {
            return Ok(true);
        }
        return Err(fail(SwapicError::WsolState));
    }
    token_account_clean(
        view,
        &wire::WSOL_MINT,
        &consts::AUTH[shard],
        SwapicError::WsolState,
    )?;
    if token_amount(view)? != before.amount {
        return Err(fail(SwapicError::WsolState));
    }
    Ok(false)
}

pub fn process(accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    require_top_level()?;
    if accounts.len() < ROUTE {
        return Err(fail(SwapicError::ShortData));
    }
    let a: &[AccountView] = accounts;
    let args = ArrivalArgs::decode(data).map_err(fail)?;
    read_config(&a[CONFIG], true)?;

    let shard = shard_of(key_of(&a[AUTH]), &consts::AUTH).ok_or(fail(SwapicError::WrongAuth))?;
    require(&a[VAULT], &consts::VAULT[shard], SwapicError::WrongVault)?;
    require(&a[WSOL], &consts::WSOL[shard], SwapicError::WrongVault)?;
    check_receive_pins(a)?;
    require(&a[JUPITER], &wire::JUPITER, SwapicError::WrongProgram)?;

    // the terms: the very bytes handed to CCTP, which verifies them and marks them used
    if args.message.len() != wire::ARRIVAL_MESSAGE_LEN {
        return Err(fail(SwapicError::BadMessageLength));
    }
    let message = Message::new(args.message).map_err(fail)?;
    check_header(&message)?;
    if message.destination_caller() != &consts::AUTH[shard]
        || message.mint_recipient() != &consts::VAULT[shard]
    {
        return Err(fail(SwapicError::NotForThisAuthOrVault));
    }
    let hook = SolanaEnvelope::parse(message.hook()).map_err(fail)?;
    if hook.action != SOLANA_ACTION_SWAP {
        return Err(fail(SwapicError::BadHook));
    }
    if now()? > hook.deadline as i64 {
        return Err(fail(SwapicError::DeadlinePassed));
    }
    let submitter = key_of(&a[SUBMITTER]);
    if !a[SUBMITTER].is_signer() || (submitter != &consts::CANISTER && submitter != &hook.recipient)
    {
        return Err(fail(SwapicError::NotCanisterOrRecipient));
    }
    if is_program_address(&hook.recipient)
        || consts::WSOL.contains(&hook.recipient)
        || hook.recipient == consts::CANISTER
        || hook.recipient == consts::ADMIN
    {
        return Err(fail(SwapicError::BadRecipient));
    }
    if hook.mint_out == wire::USDC_MINT || hook.min_out == 0 {
        return Err(fail(SwapicError::BadHook));
    }
    let native = hook.mint_out == wire::WSOL_MINT;
    if native {
        if !is(&a[OUTPUT], &hook.recipient) {
            return Err(fail(SwapicError::BadOutputAccount));
        }
    } else {
        check_token_output(&a[OUTPUT], &hook.recipient, &hook.mint_out)?;
    }

    let route = &a[ROUTE..];
    jupiter::check_route(route, Way::Arrival { shard })?;

    let auth_lamports = a[AUTH].lamports();
    let vault_before = token_amount(&a[VAULT])?;
    let wsol_before = read_wsol(&a[WSOL])?;

    let index = [shard as u8];
    let bump = [consts::AUTH_BUMP[shard]];
    let seeds = [
        Seed::from(wire::SEED_AUTH),
        Seed::from(&index),
        Seed::from(&bump),
    ];
    let signer = [Signer::from(&seeds)];
    receive_message(
        a,
        &a[AUTH],
        &a[VAULT],
        args.message,
        args.attestation,
        &signer,
    )?;

    let minted = token_amount(&a[VAULT])?
        .checked_sub(vault_before)
        .ok_or(fail(SwapicError::NothingMinted))?;
    if minted == 0 {
        return Err(fail(SwapicError::NothingMinted));
    }
    token_account_clean(
        &a[VAULT],
        &wire::USDC_MINT,
        &consts::AUTH[shard],
        SwapicError::VaultStateChanged,
    )?;
    check_wsol_after(&a[WSOL], &wsol_before, false, shard)?;
    auth_clean(&a[AUTH], auth_lamports)?;

    // read after the receive: a recipient that submits has just paid the `used_nonce` rent
    let out_before = output_amount(&a[OUTPUT], native)?;

    jupiter::call(route, args.jupiter, &consts::AUTH[shard], &signer)?;

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
    let closed = check_wsol_after(&a[WSOL], &wsol_before, native, shard)?;

    let mut got = output_amount(&a[OUTPUT], native)?
        .checked_sub(out_before)
        .ok_or(fail(SwapicError::BelowMinOut))?;
    if closed {
        // the closed account's rent was Swapic's, not the swap's output
        got = got
            .checked_sub(wsol_before.lamports)
            .ok_or(fail(SwapicError::BelowMinOut))?;
    }
    if got < hook.min_out {
        return Err(fail(SwapicError::BelowMinOut));
    }
    log::completed(&hook.ref_bytes, minted, got);
    Ok(())
}

#[inline(always)]
fn output_amount(view: &AccountView, native: bool) -> Result<u64, ProgramError> {
    if native {
        return Ok(view.lamports());
    }
    let data = view.try_borrow()?;
    if data.len() < 72 {
        return Err(fail(SwapicError::BadOutputAccount));
    }
    Ok(read_u64(&data, 64))
}
