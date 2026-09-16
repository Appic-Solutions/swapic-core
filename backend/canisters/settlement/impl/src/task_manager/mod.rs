use crate::storage::config;
use crate::storage::halt::is_halted;
use expiry_sweep::run_expiry_sweep;
use ic_cdk_timers::TimerId;
use replay_audit::run_replay_audit;
use std::cell::Cell;
use std::thread::LocalKey;
use std::time::Duration;

pub mod expiry_sweep;
pub mod replay_audit;

thread_local! {
    // The live timers, one slot each, so a restart replaces a timer instead of doubling it
    // and each one restarts without touching the other.
    static EXPIRY_TIMER: Cell<Option<TimerId>> = const { Cell::new(None) };
    static AUDIT_TIMER: Cell<Option<TimerId>> = const { Cell::new(None) };
}

/// A zero interval is a repeating timer with no gap between runs, which burns cycles for
/// nothing, so every configured interval is clamped to at least a second.
fn interval(seconds: u64) -> Duration {
    Duration::from_secs(seconds.max(1))
}

/// Wires both timers. Called from `init` and `post_upgrade`, because timers live in the
/// heap and an upgrade clears them.
pub fn start_timers() {
    // touch the halt cell here, in an update context, so no query is ever the first to
    // grow its stable memory
    let _ = is_halted();
    restart_expiry_timer();
    restart_audit_timer();
}

/// Puts the expiry timer on the configured interval. `set_config` calls it only when that
/// interval changed, because a restart pushes the next sweep a whole interval out.
pub fn restart_expiry_timer() {
    let every = interval(config::get().expiry_check_interval_s);
    restart(&EXPIRY_TIMER, || {
        ic_cdk_timers::set_timer_interval(every, || {
            // seconds, to match the quote expiries the sweep compares against
            run_expiry_sweep(ic_cdk::api::time() / 1_000_000_000);
        })
    });
}

/// Puts the audit timer on the configured interval. `set_config` calls it only when that
/// interval changed, because a restart pushes the next audit a whole interval out.
pub fn restart_audit_timer() {
    let every = interval(config::get().replay_audit_interval_s);
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
