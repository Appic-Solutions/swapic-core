use crate::guards::require_controller;
use crate::storage::config;
use crate::task_manager;
use ic_cdk::update;
pub use settlement_api::types::config::Config;

/// Controller-only, and it takes the whole record, so editing one knob is a
/// read-modify-write: read with `get_config_full`, never with the redacted `get_config`,
/// or the write puts "***" into `rpc_urls` and the canister loses its rpc access.
#[update]
pub fn set_config(new: Config) -> Result<(), String> {
    require_controller()?;
    let changed = config::set(new)?;
    // after the write, so a timer reads its new interval, and only the timer whose interval
    // moved, since a restart pushes its next run a whole interval out. A trap here rolls
    // back the event and the write with it.
    if changed.expiry {
        task_manager::restart_expiry_timer();
    }
    if changed.audit {
        task_manager::restart_audit_timer();
    }
    Ok(())
}
