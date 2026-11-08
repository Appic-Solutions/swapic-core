//! Comparing accounts with what they must be, and the small reads every instruction shares.

use crate::consts;
use crate::fail;
use pinocchio::error::ProgramError;
use pinocchio::sysvars::clock::Clock;
use pinocchio::sysvars::Sysvar;
use pinocchio::{AccountView, Address, ProgramResult};
use swapic_solana_wire::args::Config;
use swapic_solana_wire::consts as wire;
use swapic_solana_wire::errors::SwapicError;
use swapic_solana_wire::Pubkey;

pub const PROGRAM_ADDRESS: Address = Address::new_from_array(consts::PROGRAM_ID);
pub const TOKEN_ADDRESS: Address = Address::new_from_array(wire::TOKEN_PROGRAM);
pub const TOKEN_2022_ADDRESS: Address = Address::new_from_array(wire::TOKEN_2022_PROGRAM);
pub const SYSTEM_ADDRESS: Address = Address::new_from_array(wire::SYSTEM_PROGRAM);
pub const ATA_ADDRESS: Address = Address::new_from_array(wire::ATA_PROGRAM);
pub const JUPITER_ADDRESS: Address = Address::new_from_array(wire::JUPITER);
pub const TMM_ADDRESS: Address = Address::new_from_array(wire::TMM);
pub const MT_ADDRESS: Address = Address::new_from_array(wire::MT);

#[inline(always)]
pub fn key_of(view: &AccountView) -> &Pubkey {
    view.address().as_array()
}

#[inline(always)]
pub fn is(view: &AccountView, key: &Pubkey) -> bool {
    key_of(view) == key
}

#[inline(always)]
pub fn require(view: &AccountView, key: &Pubkey, error: SwapicError) -> ProgramResult {
    if is(view, key) {
        Ok(())
    } else {
        Err(fail(error))
    }
}

/// Rule 1 of design 4.5: no route AMM can re-enter Swapic carrying a signer's privilege.
#[inline(always)]
pub fn require_top_level() -> ProgramResult {
    #[cfg(target_os = "solana")]
    {
        // SAFETY: a syscall that takes nothing and returns a number
        if unsafe { pinocchio::syscalls::sol_get_stack_height() } != 1 {
            return Err(fail(SwapicError::NotTopLevel));
        }
    }
    Ok(())
}

#[inline(always)]
pub fn now() -> Result<i64, ProgramError> {
    Ok(Clock::get()?.unix_timestamp)
}

#[inline(always)]
pub fn read_u64(bytes: &[u8], at: usize) -> u64 {
    let mut word = [0u8; 8];
    word.copy_from_slice(&bytes[at..at + 8]);
    u64::from_le_bytes(word)
}

/// A token account's amount. The layout is the same for Token and Token-2022.
#[inline(always)]
pub fn token_amount(view: &AccountView) -> Result<u64, ProgramError> {
    let data = view.try_borrow()?;
    if data.len() < 72 {
        return Err(fail(SwapicError::BadOutputAccount));
    }
    Ok(read_u64(&data, 64))
}

/// The config, pinned and owned by the program; `running` also refuses a paused one.
pub fn read_config(view: &AccountView, running: bool) -> Result<Config, ProgramError> {
    require(view, &consts::CONFIG, SwapicError::WrongConfig)?;
    if !view.owned_by(&PROGRAM_ADDRESS) {
        return Err(fail(SwapicError::WrongConfig));
    }
    let data = view.try_borrow()?;
    let config = Config::parse(&data).map_err(fail)?;
    if running && config.paused {
        return Err(fail(SwapicError::Paused));
    }
    Ok(config)
}

/// The shard whose address this is in a list of four.
#[inline(always)]
pub fn shard_of(key: &Pubkey, list: &[Pubkey; 4]) -> Option<usize> {
    list.iter().position(|k| k == key)
}

/// An address no user's money may go to: it is one of the program's own.
pub fn is_program_address(key: &Pubkey) -> bool {
    *key == consts::PROGRAM_ID
        || *key == consts::CONFIG
        || *key == consts::RENT
        || shard_of(key, &consts::AUTH).is_some()
        || shard_of(key, &consts::VAULT).is_some()
}
