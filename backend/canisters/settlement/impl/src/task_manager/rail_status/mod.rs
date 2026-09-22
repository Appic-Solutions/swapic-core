//! The rail status timer: the engine's tick, on the interval the config names as
//! `rail_status_max_age`. A repeating timer rather than a one-shot one, because a swap's
//! next move depends on what the outbox and the watcher did meanwhile, and there is no
//! event to arm on when a receipt lands or an attestation arrives. Each tick runs the
//! engine under its own guard (rule A7), so a tick that outlives the interval is followed
//! by an empty one rather than a second one over the same swaps.

use crate::engine;
use crate::storage::config;
use ic_cdk_timers::TimerId;
use std::cell::Cell;
use std::time::Duration;

thread_local! {
    // Heap by necessity, like every other timer id: an upgrade invalidates it anyway.
    static RAIL_STATUS_TIMER: Cell<Option<TimerId>> = const { Cell::new(None) };
}

/// How often the engine ticks: the interval `config` names, held to the floor every timer
/// here is held to, so a knob of zero puts the engine on one tick a second and not on
/// every round of the subnet. The config is the caller's, so the decision is testable
/// without a canister.
fn every(config: &types::Config) -> Duration {
    super::interval(config.rail_status_max_age)
}

/// Puts the engine on the configured interval. `set_config` calls it only when that
/// interval changed, because a restart pushes the next tick a whole interval out.
pub fn restart_rail_status_timer() {
    let every = every(&config::get());
    super::restart(&RAIL_STATUS_TIMER, || {
        ic_cdk_timers::set_timer_interval(every, || ic_cdk::spawn(run()))
    });
}

/// One tick: the engine over every open swap. What it did is returned by the engine and
/// dropped here, the way the sweep's summary is; a query surface for it is a later plan.
async fn run() {
    engine::drive_all().await;
}

#[cfg(test)]
mod tests;
