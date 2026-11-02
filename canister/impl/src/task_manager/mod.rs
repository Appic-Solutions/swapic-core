//! The canister's timers. Two repeat: the hourly pass (`books_check_interval`) and the
//! canister's own read of Base (`arrival_backup`). Everything else is a one-shot armed while
//! there is work: the engine's drive and backstop, the outbox, and the read of Base while it
//! is behind. Timers live on the heap, so `init` and `post_upgrade` start them again.

use crate::storage::config;
use ic_cdk_timers::TimerId;
use std::cell::Cell;
use std::thread::LocalKey;
use std::time::Duration;

pub mod backstop;
pub mod outbox;
pub mod pool_arrivals;
pub mod rebalance;

thread_local! {
    // Heap by necessity: a timer id is a runtime handle that an upgrade invalidates anyway.
    // One slot per timer, so a restart replaces a timer instead of doubling it.
    static HOURLY_TIMER: Cell<Option<TimerId>> = const { Cell::new(None) };
    static POOL_ARRIVALS_TIMER: Cell<Option<TimerId>> = const { Cell::new(None) };
    static POOL_ARRIVALS_AGAIN: Cell<Option<TimerId>> = const { Cell::new(None) };
}

/// A zero interval is a repeating timer with no gap between runs, which burns cycles for
/// nothing, so every configured interval is clamped to at least a second.
fn interval(every: Duration) -> Duration {
    every.max(types::config::TIMER_FLOOR)
}

/// Wires every repeating timer. Called from `init` and `post_upgrade`, because timers live
/// in the heap and an upgrade clears them.
pub fn start_timers() {
    restart_hourly_timer();
    restart_pool_arrivals_timer();
}

/// Puts the hourly pass on the configured interval (`books_check_interval`). `set_config`
/// calls it only when that interval changed, because a restart pushes the next pass a
/// whole interval out.
pub fn restart_hourly_timer() {
    let every = interval(config::get().books_check_interval.0);
    restart(&HOURLY_TIMER, || {
        ic_cdk_timers::set_timer_interval(every, || ic_cdk::spawn(hourly()))
    });
}

/// The hourly pass. First the safety, before any await so a check that fails or traps
/// still does it: the backstop armed, and the outbox armed if anything is in flight, in
/// case a one-shot was lost. Then the balance check, the books read for a move, and the
/// ICP sweeps whose answer was lost, asked again.
async fn hourly() {
    backstop::arm();
    outbox::arm_if_work_is_pending();
    crate::books::check_pool_books().await;
    rebalance::tick().await;
    crate::sweep::icp::retry_lost().await;
}

/// Puts the canister's own read of Base on its settings' interval (`arrival_backup_s`, the
/// default while none are set), and reads once now. `set_pool_settings` calls it, so a
/// pool's first read starts at the head it gets its settings at.
pub fn restart_pool_arrivals_timer() {
    let every = interval(
        crate::storage::pool::ledger()
            .settings
            .unwrap_or_default()
            .arrival_backup,
    );
    restart(&POOL_ARRIVALS_TIMER, || {
        ic_cdk_timers::set_timer_interval(every, || ic_cdk::spawn(pool_arrivals::run_backup()))
    });
    read_base_again();
}

/// One more read of Base a second from now: the read is behind and moved last time.
pub fn read_base_again() {
    restart(&POOL_ARRIVALS_AGAIN, || {
        ic_cdk_timers::set_timer(types::config::TIMER_FLOOR, || {
            ic_cdk::spawn(pool_arrivals::run_backup())
        })
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
