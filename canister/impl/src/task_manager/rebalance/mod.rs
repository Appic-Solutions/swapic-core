//! The rebalancing's tick, on the engine's backstop while work is open and on the hourly
//! pass: the open moves driven a step on (their routes, their signatures, the minter's call,
//! the landing and arrival reads, the sweep), then the books read for a move to open. The
//! books are read after every pool movement (the pockets changed since the last read), every
//! 15 minutes while ticks come, and while a payout waits in a queue.

#[cfg(test)]
mod tests;

use crate::pool;
use crate::storage::halt::is_halted;
use std::time::Duration;
use types::pool::Pockets;
use types::Timestamp;

/// How often the books are read for a move when nothing moved.
pub const CHECK_EVERY: Duration = Duration::from_secs(15 * 60);

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
    // the last read is kept in stable memory (C-09): an upgrade reads no extra time
    let read = crate::storage::timer_state::update(|state| {
        let read = due(state.last_check, now, ledger.pockets, queued);
        if read {
            state.last_check = Some((now, ledger.pockets));
        }
        (read, read)
    });
    if read {
        pool::rebalance::trigger::open_if_needed(now);
    }
}
