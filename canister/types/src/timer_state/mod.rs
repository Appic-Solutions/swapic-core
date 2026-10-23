//! What the canister's doors and timers remember between calls (C-09): the arrival reports
//! let through for callers other than the watcher, when each Eco move to Base was last looked
//! for on the Portals, the books' last read for a move and the pockets it saw, and the queue's
//! head already alerted on. In stable memory like all the canister's state, so an upgrade
//! forgets none of it: no extra burst of reports, no extra Portal read, no repeated alert.

#[cfg(test)]
mod tests;

use crate::hash::{PoolRef, TxHash};
use crate::numeric::Timestamp;
use crate::pool::Pockets;
use minicbor::{Decode, Encode};
use std::collections::BTreeMap;
use std::time::Duration;

/// The most reports a minute from every caller but the watcher, together.
pub const PER_MINUTE: usize = 10;

/// The most attempts an hour at one `base_tx` from every caller but the watcher.
pub const PER_TX_PER_HOUR: usize = 3;

const MINUTE: Duration = Duration::from_secs(60);
const HOUR: Duration = Duration::from_secs(3_600);

/// The arrival reports let through: the last minute's, and each `base_tx`'s last hour's.
#[derive(Clone, Debug, Default, PartialEq, Eq, Encode, Decode)]
pub struct Limiter {
    #[n(0)]
    pub recent: Vec<Timestamp>,
    #[n(1)]
    pub by_tx: BTreeMap<TxHash, Vec<Timestamp>>,
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
        self.recent.push(now);
        self.by_tx.entry(tx).or_default().push(now);
        Ok(())
    }
}

/// Everything the doors and the timers remember, one stable cell.
#[derive(Clone, Debug, Default, PartialEq, Eq, Encode, Decode)]
pub struct TimerState {
    /// The arrival reports let through for callers other than the watcher.
    #[n(0)]
    pub reports: Limiter,
    /// When each Eco move to Base was last looked for on the Portals, by move id.
    #[n(1)]
    pub unfilled_reads: BTreeMap<u64, Timestamp>,
    /// When the books were last read for a move, and the pockets they saw.
    #[n(2)]
    pub last_check: Option<(Timestamp, Pockets)>,
    /// The queue's head already alerted on, so a head that waits raises one line.
    #[n(3)]
    pub alerted_head: Option<PoolRef>,
}

crate::storable_as_cbor!(TimerState);
