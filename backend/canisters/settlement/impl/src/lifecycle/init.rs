use crate::storage;
use crate::task_manager;
use ic_cdk::init;

#[init]
fn init() {
    storage::init();
    task_manager::start_timers();
}
