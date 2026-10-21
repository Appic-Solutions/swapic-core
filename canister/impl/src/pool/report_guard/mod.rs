//! The cost guard on the arrival reports anyone may make (`note_pool_arrival`,
//! `note_move_arrival`, P11e): each costs the canister a receipt outcall of up to 512 KB, so a
//! caller other than the watcher is held to at most [`PER_MINUTE`] reports a minute in total
//! and a given `base_tx` to at most [`PER_TX_PER_HOUR`] attempts an hour, refused before any
//! outcall. The watcher's principal is never limited. Heap state: an upgrade resets it, which
//! costs at most one more minute's worth.

#[cfg(test)]
mod tests;

use crate::storage::roles;
use settlement_api::types::pool::PoolError;
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::time::Duration;
use types::{Timestamp, TxHash};

/// The most reports a minute from every caller but the watcher, together.
pub const PER_MINUTE: usize = 10;

/// The most attempts an hour at one `base_tx` from every caller but the watcher.
pub const PER_TX_PER_HOUR: usize = 3;

const MINUTE: Duration = Duration::from_secs(60);
const HOUR: Duration = Duration::from_secs(3_600);

/// The reports let through: the last minute's, and each `base_tx`'s last hour's.
#[derive(Debug, Default)]
pub struct Limiter {
    recent: VecDeque<Timestamp>,
    by_tx: BTreeMap<TxHash, Vec<Timestamp>>,
}

impl Limiter {
    /// Lets one report of `tx` through at `now`, or answers why not; a refused report counts
    /// for nothing.
    pub fn admit(&mut self, tx: TxHash, now: Timestamp) -> Result<(), String> {
        let within = |at: &Timestamp, span: Duration| now.saturating_duration_since(*at) < span;
        self.recent.retain(|at| within(at, MINUTE));
        self.by_tx.retain(|_, attempts| {
            attempts.retain(|at| within(at, HOUR));
            !attempts.is_empty()
        });
        if self.recent.len() >= PER_MINUTE {
            return Err(format!(
                "at most {PER_MINUTE} arrival reports a minute are read for callers other than the watcher: ask again later"
            ));
        }
        let attempts = self.by_tx.get(&tx).map_or(0, Vec::len);
        if attempts >= PER_TX_PER_HOUR {
            return Err(format!(
                "this transaction was reported {PER_TX_PER_HOUR} times within the hour: ask again later"
            ));
        }
        self.recent.push_back(now);
        self.by_tx.entry(tx).or_default().push(now);
        Ok(())
    }
}

thread_local! {
    static LIMITER: RefCell<Limiter> = RefCell::new(Limiter::default());
}

/// The guard for a report of `base_tx` by the caller at `now`: the watcher always passes.
pub fn admit(base_tx: TxHash, now: Timestamp) -> Result<(), PoolError> {
    if roles::get().watcher == Some(ic_cdk::api::caller()) {
        return Ok(());
    }
    LIMITER
        .with(|limiter| limiter.borrow_mut().admit(base_tx, now))
        .map_err(PoolError::Unavailable)
}
