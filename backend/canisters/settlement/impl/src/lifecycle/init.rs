use crate::storage::{self, config, roles};
use crate::task_manager;
use ic_cdk::{init, trap};
pub use settlement_api::types::init::InitArg;

/// Installs straight into service. The arg's config and roles go through the storage paths
/// `set_config` and `set_roles` use, so each is validated the same way and lands with its
/// `ConfigChanged` or `RolesChanged` event. Any refusal traps, and a trap here fails the
/// install.
#[init]
fn init(arg: InitArg) {
    storage::init();
    let InitArg {
        config: wire,
        quoter,
        watcher,
    } = arg;
    let new = types::Config::try_from(wire)
        .unwrap_or_else(|error| trap(&format!("install refused, invalid config: {error}")));
    // the intervals it reports are for restarting timers, and none run yet
    config::set(new)
        .unwrap_or_else(|error| trap(&format!("install refused, config not set: {error}")));
    roles::set_roles(quoter, watcher)
        .unwrap_or_else(|error| trap(&format!("install refused, roles not set: {error}")));
    // after the config is written, so the timers start on its intervals
    task_manager::start_timers();
}
