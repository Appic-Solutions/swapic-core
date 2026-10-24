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
pub mod pool_arrivals;
pub mod rail_status;
pub mod rebalance;
pub mod replay_audit;

pub use rail_status::restart_rail_status_timer;

thread_local! {
    // Heap by necessity: a timer id is a runtime handle that an upgrade invalidates anyway.
    // One slot per timer, so a restart replaces a timer instead of doubling it.
    static EXPIRY_TIMER: Cell<Option<TimerId>> = const { Cell::new(None) };
    static AUDIT_TIMER: Cell<Option<TimerId>> = const { Cell::new(None) };
    static BOOKS_TIMER: Cell<Option<TimerId>> = const { Cell::new(None) };
    static POOL_ARRIVALS_TIMER: Cell<Option<TimerId>> = const { Cell::new(None) };
    static PAYOUT_TIMER: Cell<Option<TimerId>> = const { Cell::new(None) };
    static REBALANCE_TIMER: Cell<Option<TimerId>> = const { Cell::new(None) };
}

/// How often the payout timer serves the Base queue and walks the payouts' deadlines.
pub const PAYOUT_EVERY: Duration = Duration::from_secs(60);

/// A zero interval is a repeating timer with no gap between runs, which burns cycles for
/// nothing, so every configured interval is clamped to at least a second.
fn interval(every: Duration) -> Duration {
    every.max(Duration::from_secs(1))
}

/// Wires every repeating timer. Called from `init` and `post_upgrade`, because timers live
/// in the heap and an upgrade clears them.
pub fn start_timers() {
    restart_expiry_timer();
    restart_audit_timer();
    restart_rail_status_timer();
    restart_books_timer();
    restart_pool_arrivals_timer();
    restart_payout_timer();
    restart_rebalance_timer();
}

/// The rebalancing's timer, every minute (`rebalance::tick`).
pub fn restart_rebalance_timer() {
    restart(&REBALANCE_TIMER, || {
        ic_cdk_timers::set_timer_interval(rebalance::REBALANCE_EVERY, || {
            ic_cdk::spawn(rebalance::tick())
        })
    });
}

/// The payout timer, every minute: the engine's drive, which serves the Base queue, drives
/// the ICP swaps, undoes a reverted payout once its order is known unused, and undoes a
/// payout at its deadline. Every line that moves a payout arms the same drive at once; this is the clock.
pub fn restart_payout_timer() {
    restart(&PAYOUT_TIMER, || {
        ic_cdk_timers::set_timer_interval(PAYOUT_EVERY, || {
            ic_cdk::spawn(async {
                crate::engine::drive_all().await;
            })
        })
    });
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

/// Puts the balance check on the configured interval (`books_check_interval`). `set_config`
/// calls it only when that interval changed, because a restart pushes the next check a
/// whole interval out.
pub fn restart_books_timer() {
    let every = interval(config::get().books_check_interval.0);
    restart(&BOOKS_TIMER, || {
        ic_cdk_timers::set_timer_interval(every, || ic_cdk::spawn(crate::books::check_balances()))
    });
}

/// Puts the pool's own read of Base on its settings' interval (`arrival_backup_s`, the
/// default while none are set). `set_pool_settings` calls it.
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
