//! The outbox pass: hand what is queued to the chains, then read what came back.
//!
//! A one-shot timer rather than a repeating one, because a canister with nothing in flight
//! has nothing to do: the pass is armed when a transaction is queued, and it re-arms itself
//! while the outbox is not empty. `post_upgrade` arms it again if anything is still in
//! flight, which is rule A9.
//!
//! Rule A7: one pass at a time. A pass awaits providers, so a timer that fired while one
//! was in flight would read the same entries and send them twice; the guard is released on
//! drop, including on a trap.

use crate::storage::events::read_state;
use crate::storage::halt::is_halted;
use crate::storage::{config, outbox};
use crate::tx;
use ic_cdk_timers::TimerId;
use std::cell::Cell;
use types::Timestamp;

thread_local! {
    // Heap by necessity, like every other timer id: an upgrade invalidates it anyway. The
    // instant beside the id is when that timer was armed to fire, which is what says
    // whether it still lies ahead: see `still_waiting`.
    static FLUSH_TIMER: Cell<Option<(TimerId, Timestamp)>> = const { Cell::new(None) };
    // A7: set while a pass is running, cleared when its guard drops.
    static RUNNING: Cell<bool> = const { Cell::new(false) };
}

/// Held for the length of one pass, so two passes can never overlap.
struct PassGuard;

impl PassGuard {
    /// A guard, or nothing at all when a pass is already running.
    fn take() -> Option<Self> {
        RUNNING.with(|running| {
            if running.get() {
                return None;
            }
            running.set(true);
            Some(Self)
        })
    }
}

impl Drop for PassGuard {
    fn drop(&mut self) {
        RUNNING.with(|running| running.set(false));
    }
}

/// Whether the recorded timer still lies ahead, which is the only state in which arming
/// another one would be arming a second pass.
///
/// A recorded id alone cannot say. The callback runs as its own message, so anything it
/// does before its first await is undone if it traps, and a callback that cleared the slot
/// and then trapped would put back the id of a timer that has already fired: `arm` would
/// see it forever and no pass would ever run again. The instant the timer was armed for is
/// not undone by anything, because it is written when the timer is created, so it is what
/// the decision is made on. Arming a second timer for an instant that has passed costs at
/// worst one extra pass, and a pass with nothing to do returns immediately.
fn still_waiting(due: Option<Timestamp>, now: Timestamp) -> bool {
    due.is_some_and(|due| due > now)
}

/// Whether a pass is on its way: the recorded timer still lies ahead.
pub fn is_armed() -> bool {
    let now = Timestamp::from_nanos(ic_cdk::api::time());
    let due = FLUSH_TIMER.with(|timer| timer.get()).map(|(_, due)| due);
    still_waiting(due, now)
}

/// Puts the next pass on the batch window, unless one is already on its way. Called when a
/// transaction is queued, and again at the end of every pass that leaves work behind.
pub fn arm() {
    if is_armed() {
        return;
    }
    let now = Timestamp::from_nanos(ic_cdk::api::time());
    let window = config::get().batch_window;
    let timer = ic_cdk_timers::set_timer(window, || ic_cdk::spawn(run()));
    let due = Timestamp::from_nanos(now.as_nanos().saturating_add(window.as_nanos() as u64));
    FLUSH_TIMER.with(|slot| slot.set(Some((timer, due))));
}

/// Whether a pass still has work: bytes on their way to a chain, or a nonce that was
/// handed out and never signed for. An upgrade in the middle of a send leaves only the
/// second, and rule A5 is what makes it work the pass cannot skip.
fn work_is_pending() -> bool {
    !outbox::is_empty() || read_state(|state| !state.unsigned_nonces().is_empty())
}

/// Arms the pass if anything is still in flight. Called from `post_upgrade`, because an
/// upgrade clears every timer and a queued transaction, or an allocation waiting for its
/// cancel, would otherwise sit forever (A9).
pub fn arm_if_work_is_pending() {
    if work_is_pending() {
        arm();
    }
}

/// One pass: end every allocation that lost its transaction, broadcast what is queued, then
/// read what the chains did with what was already out. Re-arms itself while anything is
/// still in flight.
///
/// The cancel comes first so the nonce it spends goes out in this pass's own batch rather
/// than waiting a window for the next one.
async fn run() {
    let Some(_guard) = PassGuard::take() else {
        // a pass is already running and will re-arm when it is done
        return;
    };
    tx::cancel_stranded().await;
    tx::flush().await;
    tx::check_open().await;
    // a halted canister creates nothing (the two passes that would return at once) but
    // keeps reading what the chains do with the bytes already out, so an operator
    // investigating a divergence sees attempts close as they mine: while anything is
    // sent, the pass re-arms whatever the halt says. What it does not do is spin on work
    // it may not touch: a halt with only queued or unsigned work rests until `set_halted`
    // arms it again. The halt is read after the passes, because it can land during their
    // awaits.
    if outbox::any_sent() || (!is_halted() && work_is_pending()) {
        arm();
    }
}

#[cfg(test)]
mod tests;
