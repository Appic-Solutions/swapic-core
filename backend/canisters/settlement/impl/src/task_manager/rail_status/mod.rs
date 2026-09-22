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

thread_local! {
    // Heap by necessity, like every other timer id: an upgrade invalidates it anyway.
    static RAIL_STATUS_TIMER: Cell<Option<TimerId>> = const { Cell::new(None) };
}

/// Puts the engine on the configured interval. `set_config` calls it only when that
/// interval changed, because a restart pushes the next tick a whole interval out.
pub fn restart_rail_status_timer() {
    let every = super::interval(config::get().rail_status_max_age);
    super::restart(&RAIL_STATUS_TIMER, || {
        ic_cdk_timers::set_timer_interval(every, || ic_cdk::spawn(run()))
    });
}

/// One tick: the engine over every open swap. What it did is returned by the engine and
/// dropped here, the way the sweep's summary is; a query surface for it is a later plan.
async fn run() {
    engine::drive_all().await;
}

/// Whether the engine's timer is wired. Only a canister can wire one, so this is what a
/// unit test can see of the slot.
#[cfg(test)]
pub(crate) fn is_wired() -> bool {
    RAIL_STATUS_TIMER.with(|slot| slot.get().is_some())
}

#[cfg(test)]
mod tests;
