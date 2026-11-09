//! What the canister's doors and timers remember between calls: the arrival reports
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
/// every node makes, so strangers cannot drain the canister's cycles with them. Users
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
    /// The last hour's reports of transfers the canister awaits terms for, by `base_tx`: a lane
    /// of their own, so no stranger's report spends it, and still bounded, since each costs a
    /// read every node makes. None for a state written before there were any.
    #[n(2)]
    pub awaited: Option<BTreeMap<TxHash, Vec<Timestamp>>>,
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
        self.admit_tx(tx, now)?;
        self.recent.push(now);
        Ok(())
    }

    /// Lets one report through that brings the terms of a transfer of `tx` the canister awaits
    /// (audit C16): held to [`PER_TX_PER_HOUR`] of its own, never to the counts a stranger's
    /// reports fill.
    pub fn admit_awaited(&mut self, tx: TxHash, now: Timestamp) -> Result<(), String> {
        let awaited = self.awaited.get_or_insert_with(BTreeMap::new);
        awaited.retain(|_, attempts| {
            attempts.retain(|at| now.saturating_duration_since(*at) < HOUR);
            !attempts.is_empty()
        });
        let attempts = awaited.entry(tx).or_default();
        if attempts.len() >= PER_TX_PER_HOUR {
            return Err(format!(
                "these terms were reported {PER_TX_PER_HOUR} times within the hour: ask again later"
            ));
        }
        attempts.push(now);
        Ok(())
    }

    fn admit_tx(&mut self, tx: TxHash, now: Timestamp) -> Result<(), String> {
        let attempts = self.by_tx.get(&tx).map_or(0, Vec::len);
        if attempts >= PER_TX_PER_HOUR {
            return Err(format!(
                "this transaction was reported {PER_TX_PER_HOUR} times within the hour: ask again later"
            ));
        }
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
    /// The ICP sweeps whose transfer's answer was lost, by sweep id.
    #[n(8)]
    pub icp_sweeps: BTreeMap<u64, crate::sweep::IcpSweepAsk>,
    /// Each finalized-tag chain's finalized block as the outbox last read it, and when it
    /// last moved.
    #[n(9)]
    pub finalized_seen: BTreeMap<ChainId, FinalizedSeen>,
    /// When each ck payout last asked or read the minter, by payout id: none for a state
    /// written before there were any.
    #[n(10)]
    pub ck_reads: Option<BTreeMap<u64, Timestamp>>,
    /// The doors' pulls whose outcome nobody could tell (audit C15): nothing is written of
    /// them, so a human checks the ledger by the memo and gives back what moved. The watcher
    /// alerts each once. None for a state written before there were any.
    #[n(11)]
    pub pulls_to_check: Option<Vec<PullToCheck>>,
    /// The transactions of Across refunds to the canister its read of Base found while a
    /// payout they may pay back waited for its fill read: kept aside and read again each pass,
    /// so the read moves on past them (BSC wave 2). None for a state written before there were
    /// any.
    #[n(12)]
    pub across_refunds_waiting: Option<Vec<crate::hash::TxHash>>,
}

/// The most Across refunds the read of Base keeps aside: past it, the read stops before the
/// next one, with an alert.
pub const MAX_WAITING_REFUNDS: usize = 32;

/// The most pulls to check kept: the oldest goes first.
pub const MAX_PULLS_TO_CHECK: usize = 100;

/// One pull to check by hand: what it asked of which ledger, from whom.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub struct PullToCheck {
    #[n(0)]
    pub at: Timestamp,
    #[n(1)]
    pub ledger: crate::pool::PrincipalBytes,
    #[n(2)]
    pub from: crate::pool::IcpAccount,
    #[n(3)]
    pub amount: crate::TokenAmount,
    #[cbor(n(4), with = "minicbor::bytes")]
    pub memo: [u8; 32],
    /// The door, and whether it was the pull or the give-back that ended unclear.
    #[n(5)]
    pub what: String,
    #[n(6)]
    pub why: String,
}

/// How long a chain's finalized block may stand still before it is a stall worth an alert:
/// Polygon's moves every few seconds.
pub const FINALIZED_STALL: Duration = Duration::from_secs(10 * 60);

/// How often the outbox reads a chain's finalized block, slower than its receipts: between
/// reads the last number read stands, which is never above the chain's.
pub const FINALIZED_PACE: Duration = Duration::from_secs(6);

/// A finalized-tag chain's finalized block: the last number a read gave, and when it last
/// moved (or when the clock last started: at the first read, and when the chain waits again
/// after it was idle).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct FinalizedSeen {
    #[n(0)]
    pub number: Option<BlockNumber>,
    #[n(1)]
    pub moved_at: Timestamp,
    /// Whether this stall has raised its alert already.
    #[n(2)]
    pub alerted: bool,
    /// When a read was last tried.
    #[n(3)]
    pub tried_at: Option<Timestamp>,
}

impl FinalizedSeen {
    /// Before any read: the clock starts at the first try.
    pub fn first(now: Timestamp) -> Self {
        Self {
            number: None,
            moved_at: now,
            alerted: false,
            tried_at: None,
        }
    }

    /// Something on the chain waits again from `now`, after nothing did: the clock starts
    /// here, whatever the block did while the chain was idle.
    pub fn restart(&mut self, now: Timestamp) {
        self.moved_at = now;
        self.alerted = false;
    }

    /// One read at `now`, a number or a failure: a higher number moves the clock; anything
    /// else leaves it, so a provider that never serves the tag stalls as surely as a chain
    /// whose finality stopped. Answers whether this read raises the stall's one alert.
    pub fn observe(&mut self, read: Option<BlockNumber>, now: Timestamp) -> bool {
        self.tried_at = Some(now);
        if let Some(number) = read.filter(|number| self.number.is_none_or(|last| *number > last)) {
            self.number = Some(number);
            self.moved_at = now;
            self.alerted = false;
            return false;
        }
        let alert = self.is_stalled(now) && !self.alerted;
        self.alerted |= alert;
        alert
    }

    /// Whether the next read is due at `now`: none was tried within [`FINALIZED_PACE`].
    pub fn read_due(&self, now: Timestamp) -> bool {
        self.tried_at
            .is_none_or(|at| now.saturating_duration_since(at) >= FINALIZED_PACE)
    }

    /// Whether the finalized block has stood still for [`FINALIZED_STALL`] at `now`, counted
    /// from the clock's last start.
    pub fn is_stalled(&self, now: Timestamp) -> bool {
        now.saturating_duration_since(self.moved_at) >= FINALIZED_STALL
    }
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
