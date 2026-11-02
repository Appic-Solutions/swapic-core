//! Writes to the canister's USDC books.

use crate::guards::require_controller;
use ic_cdk::update;
use types::books::BookStep;

/// Controller-only. Pauses the pool (`true`), or reopens it (`false`), which also clears a
/// shortfall the last balance check latched, once someone has looked at it. Recorded in
/// the log on the pool chain's books; the pool's guard reads it.
#[update]
pub fn set_pool_paused(paused: bool) -> Result<(), String> {
    require_controller().map_err(|e| format!("{e:?}"))?;
    let chain = crate::storage::config::get()
        .pool_chain
        .ok_or("no pool chain is configured")?;
    crate::books::record(chain, BookStep::PoolPaused { paused }).map_err(|e| e.to_string())
}
