//! Rule 9 of design 4.5: no standing rights on Swapic's accounts. Amounts can return to their
//! baseline while a route leaves a new owner, a delegate or a close authority behind, and any of
//! those would strand every later arrival to the shard.

use crate::accounts::{read_u64, SYSTEM_ADDRESS, TOKEN_ADDRESS};
use crate::fail;
use pinocchio::{AccountView, ProgramResult};
use swapic_solana_wire::errors::SwapicError;
use swapic_solana_wire::Pubkey;

const TOKEN_ACCOUNT_LEN: usize = 165;
const STATE_INITIALIZED: u8 = 1;

/// The account is an initialized classic token account of this mint and owner, with no delegate
/// and no close authority.
pub fn token_account_clean(view: &AccountView, mint: &Pubkey, owner: &Pubkey, error: SwapicError) -> ProgramResult {
    if !view.owned_by(&TOKEN_ADDRESS) {
        return Err(fail(error));
    }
    let data = view.try_borrow()?;
    if data.len() != TOKEN_ACCOUNT_LEN
        || &data[..32] != mint
        || &data[32..64] != owner
        || data[72..76] != [0u8; 4]
        || data[108] != STATE_INITIALIZED
        || read_u64(&data, 121) != 0
        || data[129..133] != [0u8; 4]
    {
        return Err(fail(error));
    }
    Ok(())
}

/// `auth` is still a bare system account, its lamports what they were when the instruction started.
/// Its lamports are not required to be zero: anyone may send SOL to any address, and a shard must
/// not be stopped by a donation of a third of a dollar.
pub fn auth_clean(view: &AccountView, lamports_at_start: u64) -> ProgramResult {
    if !view.owned_by(&SYSTEM_ADDRESS) || view.data_len() != 0 || view.lamports() != lamports_at_start {
        return Err(fail(SwapicError::AuthStateChanged));
    }
    Ok(())
}
