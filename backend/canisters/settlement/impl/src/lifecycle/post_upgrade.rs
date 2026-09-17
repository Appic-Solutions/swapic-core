use crate::storage::{self, events};
use crate::task_manager;
use ic_cdk::post_upgrade;

#[post_upgrade]
fn post_upgrade() {
    // stable memory is the state, so nothing is rebuilt and nothing is migrated: this wasm
    // reads the layouts the wasm before it wrote, which `golden/storage_v1.txt` pins. The
    // touch writes the header of any structure this wasm adds.
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
}
