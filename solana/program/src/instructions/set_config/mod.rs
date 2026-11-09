//! `set_config` (tag 6), design 4.10: pauses and sets limits that move no money. Only the admin key.
//! The first call creates the account by transfer, allocate and assign, never `create_account`, so a
//! lamport donation to the address cannot block it (attack L7).

use crate::accounts::{require, PROGRAM_ADDRESS};
use crate::cctp::{system_allocate, system_assign, system_transfer};
use crate::consts;
use crate::fail;
use pinocchio::cpi::{Seed, Signer};
use pinocchio::sysvars::rent::Rent;
use pinocchio::sysvars::Sysvar;
use pinocchio::{AccountView, ProgramResult};
use swapic_solana_wire::args::{decode_set_config, Config, CONFIG_LEN};
use swapic_solana_wire::consts as wire;
use swapic_solana_wire::errors::SwapicError;

pub fn process(accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    if accounts.len() != 3 {
        return Err(fail(SwapicError::ShortData));
    }
    let incoming = decode_set_config(data).map_err(fail)?;
    {
        let a: &[AccountView] = accounts;
        require(&a[0], &consts::ADMIN, SwapicError::WrongKey)?;
        if !a[0].is_signer() {
            return Err(fail(SwapicError::WrongKey));
        }
        require(&a[1], &consts::CONFIG, SwapicError::WrongConfig)?;
        require(&a[2], &wire::SYSTEM_PROGRAM, SwapicError::WrongProgram)?;
        if !a[1].owned_by(&PROGRAM_ADDRESS) {
            let seeds = [
                Seed::from(wire::SEED_CONFIG),
                Seed::from(core::slice::from_ref(&consts::CONFIG_BUMP)),
            ];
            let signer = [Signer::from(&seeds)];
            let needed = Rent::get()?.try_minimum_balance(CONFIG_LEN)?;
            let missing = needed.saturating_sub(a[1].lamports());
            if missing > 0 {
                system_transfer(&a[0], &a[1], missing, &[])?;
            }
            system_allocate(&a[1], CONFIG_LEN as u64, &signer)?;
            system_assign(&a[1], &consts::PROGRAM_ID, &signer)?;
        }
    }
    let config = Config {
        bump: consts::CONFIG_BUMP,
        ..incoming
    };
    let mut stored = accounts[1].try_borrow_mut()?;
    if stored.len() != CONFIG_LEN {
        return Err(fail(SwapicError::WrongConfig));
    }
    stored.copy_from_slice(&config.encode());
    Ok(())
}
