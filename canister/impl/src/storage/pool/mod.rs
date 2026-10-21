//! The pool's side of the fold: its steps go through the one append path like every other
//! line (the guard runs `PoolLedger::admits`, then the line is sealed, appended and folded),
//! and these readers answer from the stable fold.

#[cfg(test)]
pub(crate) mod tests;

use crate::state::Store;
use crate::storage::events::{append_event, append_event_at, read_state, AppendError};
use std::time::Duration;
use types::hash::PoolRef;
use types::pool::{CreditKey, PoolAccount, PoolLedger, PoolStep, Registered, Shares};
use types::{ChainId, EventIndex, EventType, Timestamp};

/// How long a Base block takes, for the time its credit depth adds.
pub const BASE_BLOCK_TIME: Duration = Duration::from_secs(2);

/// Appends one pool step: refused with nothing written when the fold does not admit it.
pub fn record(step: PoolStep) -> Result<EventIndex, AppendError> {
    append_event(EventType::Pool { step })
}

/// [`record`] sealed at `at`, for a caller that already read the clock.
pub fn record_at(step: PoolStep, at: Timestamp) -> Result<EventIndex, AppendError> {
    append_event_at(EventType::Pool { step }, at)
}

pub fn ledger() -> PoolLedger {
    read_state(|state| state.pool())
}

pub fn deposit(r#ref: &PoolRef) -> Option<Registered> {
    read_state(|state| state.store().pool_deposit(r#ref))
}

/// A payout from the Base pocket, by id.
pub fn payout(id: u64) -> Option<types::pool::EvmPayout> {
    read_state(|state| state.store().pool_payout(id))
}

pub fn shares_of(account: &PoolAccount) -> Shares {
    read_state(|state| {
        state
            .store()
            .pool_share(&account.key())
            .map_or(Shares::ZERO, |line| line.shares)
    })
}

pub fn credited(key: &CreditKey) -> bool {
    read_state(|state| state.store().is_pool_credited(key))
}

/// How long after a registration's last instant of credit (its expiry and grace) an arrival
/// on time may still be read: one interval of the canister's own read of Base, and the time
/// Base takes to bury a block at the credit depth. The read keeps reading, and the sweep
/// keeps the registration open, this much longer, so an arrival on time is never expired
/// before it is read.
pub fn read_margin(arrival_backup: Duration, depth_blocks: u64) -> Duration {
    let depth = u32::try_from(depth_blocks).unwrap_or(u32::MAX);
    arrival_backup.saturating_add(BASE_BLOCK_TIME.saturating_mul(depth))
}

/// [`read_margin`] for the pool's settings and the configured credit depth on Base.
pub fn read_margin_in<S: Store>(store: &S) -> Duration {
    let backup = store
        .pool_ledger()
        .settings
        .map(|settings| settings.arrival_backup)
        .unwrap_or_default();
    let depth = crate::tx::credit_confirmations(&crate::storage::config::get(), ChainId::BASE)
        .map_or(1, |depth| depth.get());
    read_margin(backup, depth)
}

/// The registrations still waiting for their money at `now`: registered, and not past their
/// expiry, grace and read margin ([`read_margin`]).
pub fn waiting_refs(now: Timestamp) -> Vec<PoolRef> {
    read_state(|state| waiting_in(state.store(), now))
}

/// [`waiting_refs`] over any store.
pub fn waiting_in<S: Store>(store: &S, now: Timestamp) -> Vec<PoolRef> {
    let read_until = read_until(store);
    let ledger = store.pool_ledger();
    ledger
        .open
        .iter()
        .filter(|r#ref| {
            store
                .pool_deposit(r#ref)
                .is_some_and(|deposit| read_until(&deposit) >= now)
        })
        .copied()
        .collect()
}

/// The last instant a registration is read for and kept open: its expiry, grace and read
/// margin. What the canister's read of Base and the sweep both go by.
pub fn read_until<S: Store>(store: &S) -> impl Fn(&Registered) -> Timestamp {
    let grace = store
        .pool_ledger()
        .settings
        .map(|settings| settings.deposit_grace)
        .unwrap_or_default();
    let margin = read_margin_in(store);
    move |deposit| {
        deposit
            .terms
            .credit_until(grace)
            .checked_add(margin)
            .unwrap_or(Timestamp::from_nanos(u64::MAX))
    }
}
