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

use crate::storage::{config, outbox};
use crate::tx;
use ic_cdk_timers::TimerId;
use std::cell::Cell;

thread_local! {
    // Heap by necessity, like every other timer id: an upgrade invalidates it anyway.
    static FLUSH_TIMER: Cell<Option<TimerId>> = const { Cell::new(None) };
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

/// Puts the next pass on the batch window, unless one is already waiting. Called when a
/// transaction is queued, and again at the end of every pass that leaves work behind.
pub fn arm() {
    let already_armed = FLUSH_TIMER.with(|timer| {
        let armed = timer.get();
        armed.is_some()
    });
    if already_armed {
        return;
    }
    let window = config::get().batch_window;
    let timer = ic_cdk_timers::set_timer(window, || {
        FLUSH_TIMER.with(|timer| timer.set(None));
        ic_cdk::spawn(run());
    });
    FLUSH_TIMER.with(|slot| slot.set(Some(timer)));
}

/// Arms the pass if anything is still in flight. Called from `post_upgrade`, because an
/// upgrade clears every timer and a queued transaction would otherwise sit forever (A9).
pub fn arm_if_work_is_pending() {
    if !outbox::is_empty() {
        arm();
    }
}

/// One pass: broadcast what is queued, then read what the chains did with what was already
/// out. Re-arms itself while the outbox holds anything.
async fn run() {
    let Some(_guard) = PassGuard::take() else {
        // a pass is already running and will re-arm when it is done
        return;
    };
    tx::flush().await;
    tx::check_open().await;
    if !outbox::is_empty() {
        arm();
    }
}

#[cfg(test)]
mod tests;
