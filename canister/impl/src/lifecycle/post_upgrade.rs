use crate::storage::{self, events};
use crate::task_manager;
use ic_cdk::post_upgrade;

#[post_upgrade]
fn post_upgrade() {
    // an upgrade is the only path a running canister takes to a new build: it is never
    // reinstalled, because its stable memory (the log, the signed transactions, the nonces,
    // the pool) is the state. So nothing is rebuilt and nothing is migrated here, and the
    // touch writes the header of any structure this wasm adds. Every stored layout is
    // append-only and pinned by `types/golden/storage_v1.txt` (minicbor) and
    // `impl/golden/candid_cells_v1.txt` (the two candid cells); a stored value this build
    // cannot read traps below, and a trap keeps the wasm the canister runs.
    storage::init();
    // the two cells that hold candid: the roles trap above on bytes that do not decode,
    // and the flow settings here on bytes that do not decode or parse under this build's
    // rules. Neither is ever read as the defaults after an upgrade (L-08)
    if let Err(error) = storage::flow_settings::check() {
        ic_cdk::trap(&format!("upgrade refused: {error}"));
    }
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
    // and the engine's one-shot drive went with the old heap: drive once now rather than
    // leave whatever was mid-step to the backstop tick
    crate::engine::arm();
}
