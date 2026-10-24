//! The rail status timer: the engine's backstop tick, on the interval the config names as
//! `rail_status_max_age` and at least every minute, the payouts' clock: the drive serves
//! the Base queue, drives the ICP swaps, undoes a reverted payout once its order is known
//! unused, and undoes a payout at its deadline. Every line that moves a step already arms a
//! one-shot drive (`engine::arm`); this tick is for what no line announces, a deposit
//! landing on a chain or a balance arriving on a ledger, a deadline, and a read worth trying
//! again. Each tick
//! runs the engine under its own guard (rule A7), so a tick that outlives the interval is
//! followed by an empty one rather than a second one over the same payouts.

use crate::engine;
use crate::storage::config;
use ic_cdk_timers::TimerId;
use std::cell::Cell;

thread_local! {
    // Heap by necessity, like every other timer id: an upgrade invalidates it anyway.
    static RAIL_STATUS_TIMER: Cell<Option<TimerId>> = const { Cell::new(None) };
}

/// Puts the engine on the configured interval ([`types::Config::engine_every`]).
/// `set_config` calls it only when that interval changed, because a restart pushes the next
/// tick a whole interval out.
pub fn restart_rail_status_timer() {
    let every = config::get().engine_every();
    super::restart(&RAIL_STATUS_TIMER, || {
        ic_cdk_timers::set_timer_interval(every, || ic_cdk::spawn(run()))
    });
}

/// One tick: the engine's drive over the pool's payouts. What it did is returned by the
/// engine and dropped here; a query surface for it is a later plan.
async fn run() {
    engine::drive_all().await;
}
