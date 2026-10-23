//! The per-caller limit on the doors anyone may call for free, `swap_from_icp` and
//! `stake_icp` (Jeff, 2026-10-23): each costs no approval to call, and each call writes two
//! permanent lines before its pull is refused. Counted before anything is recorded; a call
//! past the limit is refused with nothing written, its count included. In stable memory
//! ([`crate::storage::callers`]).

#[cfg(test)]
mod tests;

use candid::Principal;
use settlement_api::types::pool::PoolError;
use types::pool::callers;
use types::Timestamp;

/// Lets one call of `caller` through at `now`, or refuses it with nothing written.
pub fn admit(caller: Principal, now: Timestamp) -> Result<(), PoolError> {
    let calls = callers::admit(crate::storage::callers::get(caller), now)
        .map_err(|refusal| PoolError::Unavailable(refusal.to_string()))?;
    crate::storage::callers::set(caller, calls);
    Ok(())
}
