//! The pool on the canister's side: its guard, its settings, the registration of a deposit,
//! the canister's own read of an arrival on Base, the payout on ICP, and the ICP staker's
//! stake and withdrawal. The rules are `types::pool`'s; this module reads the clock, the
//! config, the sanctions set and the chains, and appends.

pub mod approve;
pub mod arrival;
pub mod icp;
pub mod icp_swap;
pub mod legs;
pub mod orphans;
pub mod pay;
pub mod payout;
pub mod queue;
pub mod rebalance;
pub mod register;
pub mod settings;
pub mod withdraw;

use crate::books;
use crate::guards::require_not_halted;
use crate::storage::config;
use crate::storage::events::read_state;
use crate::storage::halt::is_halted;
use settlement_api::types::errors::GuardError;
use settlement_api::types::pool::{PocketView, PoolError, PoolStateView};
use types::books::PoolClosed;
use types::pool::{PoolLedger, Shares};
use types::Timestamp;

/// A line for ops reading the replica's log; nothing off the canister, where a unit test
/// runs.
pub fn log(message: &str) {
    #[cfg(target_arch = "wasm32")]
    ic_cdk::api::print(message);
    #[cfg(not(target_arch = "wasm32"))]
    let _ = message;
}

pub fn now() -> Timestamp {
    Timestamp::from_nanos(ic_cdk::api::time())
}

/// A guard's refusal in the interface's words.
pub fn guard_error(error: GuardError) -> PoolError {
    match error {
        GuardError::Halted => PoolError::Halted,
        other => PoolError::NotAllowed(format!("{other:?}")),
    }
}

/// The halt first, as every pool update runs it.
pub fn require_running() -> Result<(), PoolError> {
    require_not_halted().map_err(guard_error)
}

/// Whether the pool may move money now: the books' guard (fail closed: a halt, no pool
/// chain, a controller's pause, a latched shortfall, stale books), and settings present.
pub fn pool_closed() -> Option<String> {
    let now = now();
    let config = config::get();
    read_state(|state| {
        closed_reason(
            books::pool_status(state, &config, is_halted(), now),
            &state.pool(),
        )
    })
}

/// The pool's guard in the interface's words: `Closed(reason)`.
pub fn require_pool_open() -> Result<(), PoolError> {
    match pool_closed() {
        Some(reason) => Err(PoolError::Closed(reason)),
        None => Ok(()),
    }
}

/// Why the pool is closed, from the books' guard and the ledger.
pub fn closed_reason(books: Result<(), PoolClosed>, ledger: &PoolLedger) -> Option<String> {
    if let Err(closed) = books {
        return Some(closed.to_string());
    }
    if ledger.settings.is_none() {
        return Some("the pool has no settings".to_string());
    }
    None
}

/// `pool_state` of a ledger, the guard's answer given.
pub fn state_view(ledger: &PoolLedger, closed: Option<String>) -> PoolStateView {
    let settings = ledger.settings.clone().unwrap_or_default();
    PoolStateView {
        open: closed.is_none(),
        closed_reason: closed,
        base: PocketView::from(ledger.pockets.base),
        icp: PocketView::from(ledger.pockets.icp),
        total_assets: candid::Nat::from(ledger.total_assets()),
        total_shares: candid::Nat::from(ledger.total_shares.get()),
        tvl_cap: candid::Nat::from(settings.tvl_cap),
        per_swap_cap: candid::Nat::from(settings.per_swap_cap),
        premium_bps: settings.premium_bps,
    }
}

/// What `owned` shares are worth in the pool now.
pub fn value_of(ledger: &PoolLedger, owned: Shares) -> types::TokenAmount {
    Shares::value_of(owned, ledger.total_assets(), ledger.total_shares)
}

#[cfg(test)]
mod tests;
