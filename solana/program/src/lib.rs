//! The Swapic Solana program (design section 4). It ties Jupiter and CCTP into one transaction, so a
//! user ends with all of `tokenOut` or all of one stablecoin, and it holds no money between
//! transactions. It keeps no state but its config.

extern crate alloc;

mod accounts;
mod cctp;
mod instructions;
mod jupiter;
mod log;
mod state_check;

pub mod consts {
    include!(concat!(env!("OUT_DIR"), "/consts.rs"));
}

use pinocchio::error::ProgramError;
use pinocchio::{AccountView, Address, ProgramResult};
use swapic_solana_wire::args::{TAG_BURN, TAG_COMPLETE, TAG_RECLAIM, TAG_RELEASE, TAG_SET_CONFIG, TAG_SWAP_BURN};
use swapic_solana_wire::errors::SwapicError;

#[cfg(all(target_os = "solana", not(feature = "no-entrypoint")))]
pinocchio::entrypoint!(process);

#[cfg(all(target_os = "solana", not(feature = "no-entrypoint")))]
solana_security_txt::security_txt! {
    name: "Swapic",
    project_url: "https://swapic.xyz",
    contacts: "link:https://swapic.xyz",
    policy: "https://swapic.xyz",
    source_code: "https://github.com/asjn3e/swapic-core"
}

#[inline(always)]
pub(crate) fn fail(error: SwapicError) -> ProgramError {
    ProgramError::Custom(error.code())
}

pub fn process(_program: &Address, accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    match data.first().copied() {
        Some(TAG_SWAP_BURN) | Some(TAG_BURN) => instructions::burn::process(accounts, data),
        Some(TAG_COMPLETE) => instructions::complete::process(accounts, data),
        Some(TAG_RELEASE) => instructions::release::process(accounts, data),
        Some(TAG_RECLAIM) => instructions::reclaim::process(accounts, data),
        Some(TAG_SET_CONFIG) => instructions::set_config::process(accounts, data),
        _ => Err(fail(SwapicError::BadAction)),
    }
}
