use crate::storage::config;
use expiry_sweep::run_expiry_sweep;
use ic_cdk_timers::TimerId;
use replay_audit::run_replay_audit;
use std::cell::Cell;
use std::thread::LocalKey;
use std::time::Duration;
use types::Timestamp;

pub mod expiry_sweep;
pub mod outbox;
pub mod rail_status;
pub mod replay_audit;

pub use rail_status::restart_rail_status_timer;

thread_local! {
    // Heap by necessity: a timer id is a runtime handle that an upgrade invalidates anyway.
    // One slot per timer, so a restart replaces a timer instead of doubling it.
    static EXPIRY_TIMER: Cell<Option<TimerId>> = const { Cell::new(None) };
    static AUDIT_TIMER: Cell<Option<TimerId>> = const { Cell::new(None) };
}

/// A zero interval is a repeating timer with no gap between runs, which burns cycles for
/// nothing, so every configured interval is clamped to at least a second.
fn interval(every: Duration) -> Duration {
    every.max(Duration::from_secs(1))
}

/// Wires the three repeating timers. Called from `init` and `post_upgrade`, because timers
/// live in the heap and an upgrade clears them.
pub fn start_timers() {
    restart_expiry_timer();
    restart_audit_timer();
    restart_rail_status_timer();
}

/// Puts the expiry timer on the configured interval. `set_config` calls it only when that
/// interval changed, because a restart pushes the next sweep a whole interval out.
pub fn restart_expiry_timer() {
    let every = interval(config::get().expiry_check_interval);
    restart(&EXPIRY_TIMER, || {
        ic_cdk_timers::set_timer_interval(every, || {
            run_expiry_sweep(Timestamp::from_nanos(ic_cdk::api::time()));
        })
    });
}

/// Puts the audit timer on the configured interval. `set_config` calls it only when that
/// interval changed, because a restart pushes the next audit a whole interval out.
pub fn restart_audit_timer() {
    let every = interval(config::get().replay_audit_interval);
    restart(&AUDIT_TIMER, || {
        ic_cdk_timers::set_timer_interval(every, run_replay_audit)
    });
}

/// Clears the timer in `slot`, if any, and puts the one `start` sets in its place, so a
/// second restart replaces the timer rather than doubling its rate.
fn restart(slot: &'static LocalKey<Cell<Option<TimerId>>>, start: impl FnOnce() -> TimerId) {
    if let Some(old) = slot.take() {
        ic_cdk_timers::clear_timer(old);
    }
    slot.set(Some(start()));
}

#[cfg(test)]
mod tests;
