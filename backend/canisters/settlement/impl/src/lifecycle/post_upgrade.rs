use crate::storage::{self, events};
use crate::task_manager;
use ic_cdk::post_upgrade;

#[post_upgrade]
fn post_upgrade() {
    // stable memory is the state, so nothing is rebuilt and nothing is migrated, and the
    // touch writes the header of any structure this wasm adds. `golden/storage_v1.txt` pins
    // the layouts, but only from this wasm on: the previously deployed wasm stored
    // candid-encoded envelopes, which these types do not read, so a canister running it
    // cannot take this upgrade. It traps on the check below and keeps the wasm it has, which
    // is the right outcome; this build wants a fresh install.
    storage::init();
    // a fold out of step with its log would take the upgrade and then refuse every append,
    // so the upgrade traps instead, and a trap here leaves the old wasm running
    if let Err(error) = events::ensure_fold_in_step() {
        ic_cdk::trap(&format!(
            "upgrade refused, the stable fold is out of step with the event log: {error}"
        ));
    }
    // timers live in the heap, so an upgrade clears them and they are wired again here
    task_manager::start_timers();
    // rule A9: a transaction signed before the upgrade is still in flight, and the one-shot
    // pass that would have sent it went with the old heap
    task_manager::outbox::arm_if_work_is_pending();
}
