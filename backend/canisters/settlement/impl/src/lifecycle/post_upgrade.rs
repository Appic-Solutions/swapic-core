use crate::storage::{config, events, roles};
use crate::task_manager;
use ic_cdk::post_upgrade;

#[post_upgrade]
fn post_upgrade() {
    // the heap holds nothing the log cannot rebuild, so there is no pre_upgrade to match
    events::rebuild_state_from_log();
    // except the config and the roles, which are deploy-time truth rather than a fold of
    // the events. The pending quotes are neither: they are pre-money and come back empty.
    config::load();
    roles::load();
    // timers live in the heap, so an upgrade clears them and they are wired again here
    task_manager::start_timers();
}
