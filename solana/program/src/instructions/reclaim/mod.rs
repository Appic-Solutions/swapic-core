//! `reclaim` (tag 5), design 4.9: the event account's rent goes to Swapic. Anyone may call it; the
//! lamports go only to the canister's payer key, and CCTP checks the 5-day window and the rent payer.

use crate::accounts::{require, require_top_level};
use crate::cctp::{reclaim_event_account, system_transfer};
use crate::consts;
use crate::fail;
use crate::log;
use pinocchio::cpi::{Seed, Signer};
use pinocchio::{AccountView, ProgramResult};
use swapic_solana_wire::args::ReclaimArgs;
use swapic_solana_wire::consts as wire;
use swapic_solana_wire::errors::SwapicError;

const SIGNER: usize = 0;
const RENT: usize = 1;
const MESSAGE_TRANSMITTER: usize = 2;
const EVENT: usize = 3;
const MT: usize = 4;
const CANISTER: usize = 5;
const SYSTEM: usize = 6;

pub fn process(accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    require_top_level()?;
    if accounts.len() != 7 {
        return Err(fail(SwapicError::ShortData));
    }
    let a: &[AccountView] = accounts;
    let args = ReclaimArgs::decode(data).map_err(fail)?;
    if !a[SIGNER].is_signer() {
        return Err(fail(SwapicError::WrongKey));
    }
    require(&a[RENT], &consts::RENT, SwapicError::WrongRent)?;
    require(
        &a[MESSAGE_TRANSMITTER],
        &wire::MESSAGE_TRANSMITTER,
        SwapicError::WrongProgram,
    )?;
    require(&a[MT], &wire::MT, SwapicError::WrongProgram)?;
    require(&a[CANISTER], &consts::CANISTER, SwapicError::WrongKey)?;
    require(&a[SYSTEM], &wire::SYSTEM_PROGRAM, SwapicError::WrongProgram)?;

    let seeds = [
        Seed::from(wire::SEED_RENT),
        Seed::from(core::slice::from_ref(&consts::RENT_BUMP)),
    ];
    let before = a[RENT].lamports();
    reclaim_event_account(
        &a[RENT],
        &a[MESSAGE_TRANSMITTER],
        &a[EVENT],
        args.attestation,
        args.destination_message,
        &[Signer::from(&seeds)],
    )?;
    let delta = a[RENT]
        .lamports()
        .checked_sub(before)
        .ok_or(fail(SwapicError::WrongRent))?;
    system_transfer(&a[RENT], &a[CANISTER], delta, &[Signer::from(&seeds)])?;
    log::reclaimed(delta);
    Ok(())
}
