use crate::storage::{config, events, roles};
use crate::task_manager;
use ic_cdk::init;

#[init]
fn init() {
    // touch the log and every cell in an update context so the stable headers are
    // written here and no query is ever the first to grow stable memory
    events::rebuild_state_from_log();
    config::load();
    roles::load();
    // reads the intervals out of the config above, so it goes last
    task_manager::start_timers();
}
