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

