use crate::storage;
use crate::task_manager;
use ic_cdk::post_upgrade;

#[post_upgrade]
fn post_upgrade() {
    // everything that matters is in stable memory already, so nothing is rebuilt; the
    // touch only initializes a structure an older wasm did not have
    storage::init();
    // timers live in the heap, so an upgrade clears them and they are wired again here
    task_manager::start_timers();
}
