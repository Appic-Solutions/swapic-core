//! The rebalancing's timer, every minute: the open moves driven a step on (their routes, their
//! signatures, the minter's call, the landing and arrival reads, the sweep), then the books
//! read for a move to open. The books are read after every pool movement (the pockets changed
//! since the last read), every 15 minutes, and while a payout waits in a queue.

#[cfg(test)]
mod tests;

use crate::pool;
use crate::storage::halt::is_halted;
use std::cell::Cell;
use std::time::Duration;
use types::pool::Pockets;
use types::Timestamp;

/// How often the timer runs.
pub const REBALANCE_EVERY: Duration = Duration::from_secs(60);

/// How often the books are read for a move when nothing moved.
pub const CHECK_EVERY: Duration = Duration::from_secs(15 * 60);

thread_local! {
    // Heap by necessity, and only a clock: an upgrade loses it, and the next tick reads the
    // books at once, which is what a fresh canister should do anyway.
    static LAST_CHECK: Cell<Option<(Timestamp, Pockets)>> = const { Cell::new(None) };
}

/// Whether the books are read for a move now: never read yet, read 15 minutes ago, the
/// pockets moved since, or a payout waits in a queue.
pub fn due(
    last: Option<(Timestamp, Pockets)>,
    now: Timestamp,
    pockets: Pockets,
    queued: bool,
) -> bool {
    match last {
        None => true,
        Some((at, seen)) => {
            queued || seen != pockets || now.saturating_duration_since(at) >= CHECK_EVERY
        }
    }
}

/// One tick: the open moves, then the books.
pub async fn tick() {
    if is_halted() {
        return;
    }
    pool::rebalance::drive(pool::now()).await;
    let now = pool::now();
    let ledger = crate::storage::pool::ledger();
    let queued = !ledger.queue.0.is_empty() || ledger.lines().queue_len > 0;
    if due(LAST_CHECK.get(), now, ledger.pockets, queued) {
        LAST_CHECK.set(Some((now, ledger.pockets)));
        pool::rebalance::trigger::open_if_needed(now);
    }
}
