//! The pool's side of the fold: its steps go through the one append path like every other
//! line (the guard runs `PoolLedger::admits`, then the line is sealed, appended and folded),
//! and these readers answer from the stable fold.

#[cfg(test)]
pub(crate) mod tests;

use crate::state::Store;
use crate::storage::events::{append_event, append_event_at, read_state, AppendError};
use types::hash::PoolRef;
use types::pool::{CreditKey, PoolAccount, PoolLedger, PoolStep, Registered, Shares};
use types::{EventIndex, EventType, Timestamp};

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

/// The registrations still waiting for their money at `now`: registered, and not past their
/// expiry and grace.
pub fn waiting_refs(now: Timestamp) -> Vec<PoolRef> {
    read_state(|state| waiting_in(state.store(), now))
}

/// [`waiting_refs`] over any store.
pub fn waiting_in<S: Store>(store: &S, now: Timestamp) -> Vec<PoolRef> {
    let ledger = store.pool_ledger();
    let grace = ledger
        .settings
        .as_ref()
        .map(|settings| settings.deposit_grace)
        .unwrap_or_default();
    ledger
        .open
        .iter()
        .filter(|r#ref| {
            store
                .pool_deposit(r#ref)
                .is_some_and(|deposit| deposit.terms.credit_until(grace) >= now)
        })
        .copied()
        .collect()
}
