//! What the canister's doors and timers remember between calls (C-09): the arrival reports
//! let through for callers other than the watcher, when each Eco move to Base was last looked
//! for on the Portals, the books' last read for a move and the pockets it saw, the queue's
//! head already alerted on, the backup scan's cursor, the pockets' last balance reads, and
//! when each chain's receipts are read next. In stable memory like all the canister's state,
//! so an upgrade forgets none of it: no extra burst of reports, no extra Portal read, no
//! repeated alert.

#[cfg(test)]
mod tests;

use crate::books::BalanceCheck;
use crate::chain::ChainId;
use crate::hash::{PoolRef, TxHash};
use crate::numeric::{BlockNumber, Timestamp};
use crate::pool::{IcpCheck, Pockets};
use minicbor::{Decode, Encode};
use std::collections::BTreeMap;
use std::time::Duration;

/// The most reports a minute from every caller but the watcher, together: each costs a read
/// every node makes (M-03), so strangers cannot drain the canister's cycles with them. Users
/// report only while the watcher is down, and the canister's own read of Base credits
/// anyway.
pub const PER_MINUTE: usize = 2;

/// The most reports an hour from every caller but the watcher, together.
pub const PER_HOUR: usize = 20;

/// The most attempts an hour at one `base_tx` from every caller but the watcher.
pub const PER_TX_PER_HOUR: usize = 3;

const MINUTE: Duration = Duration::from_secs(60);
const HOUR: Duration = Duration::from_secs(3_600);

/// The arrival reports let through: the last hour's, and each `base_tx`'s last hour's.
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
        self.recent.retain(|at| within(at, HOUR));
        self.by_tx.retain(|_, attempts| {
            attempts.retain(|at| within(at, HOUR));
            !attempts.is_empty()
        });
        if self.recent.iter().filter(|at| within(at, MINUTE)).count() >= PER_MINUTE {
            return Err(format!(
                "at most {PER_MINUTE} arrival reports a minute are read for callers other than the watcher: ask again later"
            ));
        }
        if self.recent.len() >= PER_HOUR {
            return Err(format!(
                "at most {PER_HOUR} arrival reports an hour are read for callers other than the watcher: ask again later"
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
    /// How far the canister's own read of Base got.
    #[n(4)]
    pub scan_cursor: Option<ScanCursor>,
    /// The pool chain's balance reads, by chain: a read that passed is kept here and nowhere
    /// else.
    #[n(5)]
    pub books_reads: BTreeMap<ChainId, LastRead<BalanceCheck>>,
    /// The ICP pocket's balance reads.
    #[n(6)]
    pub icp_read: LastRead<IcpCheck>,
    /// When the outbox reads each chain's receipts next.
    #[n(7)]
    pub receipts_due: BTreeMap<ChainId, Timestamp>,
}

/// A pocket's balance reads: the last one that read a balance, and when one last failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct LastRead<C> {
    /// When the last read that read a balance was made, and what it read.
    #[n(0)]
    pub read: Option<(Timestamp, C)>,
    #[n(1)]
    pub failed_at: Option<Timestamp>,
}

impl<C> Default for LastRead<C> {
    fn default() -> Self {
        Self {
            read: None,
            failed_at: None,
        }
    }
}

impl<C: Copy> LastRead<C> {
    /// When a balance was last read.
    pub fn read_at(&self) -> Option<Timestamp> {
        self.read.map(|(at, _)| at)
    }

    /// What the last read read.
    pub fn last(&self) -> Option<C> {
        self.read.map(|(_, check)| check)
    }
}

impl LastRead<BalanceCheck> {
    /// The block no read may go below: the last one a read was made at. A read there
    /// dropped the fees and arrivals at or below it from the books' unseen lists, so a read
    /// further back would expect what it cannot hold.
    pub fn floor(&self) -> Option<BlockNumber> {
        self.last().map(|check| check.block)
    }
}

/// Where the canister's own read of Base stands: every transfer at or before it is done.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct ScanCursor {
    #[n(0)]
    pub block: BlockNumber,
    /// The log of `block` the read is done through, the rest of the block to read yet. None:
    /// the block is done whole.
    #[n(1)]
    pub log: Option<u64>,
}

impl ScanCursor {
    /// The cursor in order: a block, then its log, a block done whole above every log of it.
    pub fn position(&self) -> (BlockNumber, u128) {
        (self.block, self.log.map_or(u128::MAX, u128::from))
    }
}

crate::storable_as_cbor!(TimerState);
