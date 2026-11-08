//! The pool on the canister's side: its guard, its settings, the registration of a deposit,
//! the canister's own read of an arrival on Base, the payout on ICP, and the ICP staker's
//! stake and withdrawal. The rules are `types::pool`'s; this module reads the clock, the
//! config, the sanctions set and the chains, and appends.

pub mod approve;
pub mod arrival;
pub mod callers;
pub mod ck;
pub mod eco;
pub mod icp;
pub mod icp_swap;
pub mod legs;
pub mod orphans;
pub mod pay;
pub mod payout;
pub mod plan;
pub mod pull_first;
pub mod queue;
pub mod rebalance;
pub mod report_guard;
pub mod resolve;
pub mod settings;
pub mod withdraw;

use crate::books;
use crate::guards::require_not_halted;
use crate::storage::config;
use crate::storage::events::read_state;
use crate::storage::halt::is_halted;
use candid::Principal;
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

/// A controller's door's first rules, in order: refused while halted, then for anyone but a
/// controller, the refusal naming `what` only a controller does.
pub fn controller_only(halted: bool, is_controller: bool, what: &str) -> Result<(), PoolError> {
    if halted {
        return Err(PoolError::Halted);
    }
    if !is_controller {
        return Err(PoolError::NotAllowed(format!("only a controller {what}")));
    }
    Ok(())
}

/// The halt first, as every pool update runs it.
pub fn require_running() -> Result<(), PoolError> {
    require_not_halted().map_err(guard_error)
}

/// Whether the pool may move money now: the books' guard (fail closed: a halt, no pool
/// chain, a controller's pause, a latched shortfall, stale books), and settings present.
pub fn pool_closed() -> Option<String> {
    let now = now();
    let reads = crate::storage::timer_state::get();
    config::with(|config| {
        read_state(|state| {
            // the ledger read once for both
            let pool = state.pool();
            closed_reason(
                books::pool_status(state, &pool, &reads, config, is_halted(), now),
                &pool,
            )
        })
    })
}

/// Whether the pool has work open at `now` that a timer must come back to: anything the
/// payout drive acts on (`payout::Work`), a Base payout not paid, undone or stopped, a
/// queued payout, or a move not arrived and not stopped for a controller. What waits for
/// a human keeps no timer running.
pub fn work_is_open<S: crate::state::Store>(
    state: &crate::state::State<S>,
    now: Timestamp,
) -> bool {
    let ledger = state.pool();
    let lines = ledger.lines();
    !lines.open.is_empty()
        || lines.queue_len > 0
        || !payout::Work::of(state, now).is_empty()
        || rebalance::open_moves(state)
            .iter()
            .any(|item| item.stuck.is_none())
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

/// Refuses an account a request names for the anonymous principal: anyone can call as it, so
/// whatever is paid or staked to it is anyone's to take (audit C4). Checked in the shared
/// conversions, which consent runs too, so a cold signer is never told it is theirs.
pub fn not_anonymous(what: &str, owner: Principal) -> Result<(), PoolError> {
    if owner == Principal::anonymous() {
        return Err(PoolError::NotAllowed(format!(
            "{what} is owned by the anonymous principal, which anyone can spend from"
        )));
    }
    Ok(())
}

/// The doors that pay an ICP caller refuse the anonymous one in their bodies (never in the
/// conversions, which a cold signer's anonymous consent request runs too).
pub fn caller_not_anonymous(caller: Principal) -> Result<(), PoolError> {
    if caller == Principal::anonymous() {
        return Err(PoolError::NotAllowed(
            "the anonymous caller cannot use this door".to_string(),
        ));
    }
    Ok(())
}

/// Whether `principal` is sanctioned, as the sanctions set keys it (its text): the one screen
/// of a principal the ICP doors share (`swap_from_icp`, `stake_icp`, `withdraw_icp`, CR-11).
/// Fails closed: a principal whose text the set cannot key is screened as sanctioned.
pub fn principal_sanctioned(
    principal: candid::Principal,
    sanctioned: &impl Fn(&types::Address) -> bool,
) -> bool {
    principal
        .to_text()
        .parse::<types::Address>()
        .map_or(true, |text| sanctioned(&text))
}

/// What `owned` shares are worth in the pool now.
pub fn value_of(ledger: &PoolLedger, owned: Shares) -> types::TokenAmount {
    Shares::value_of(owned, ledger.total_assets(), ledger.total_shares)
}

#[cfg(test)]
mod tests;
