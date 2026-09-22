use crate::guards::require_controller;
use crate::storage::config::{self, SetConfigError as StoreError};
use crate::task_manager;
use ic_cdk::update;
pub use settlement_api::types::config::Config;
pub use settlement_api::types::errors::SetConfigError;

/// Controller-only, and it takes the whole record, so editing one knob is a
/// read-modify-write: read with `get_config_full`, never with the redacted `get_config`,
/// or the write puts "***" into `rpc_urls` and is refused.
#[update]
pub fn set_config(new: Config) -> Result<(), SetConfigError> {
    require_controller().map_err(SetConfigError::Guard)?;
    let new = types::Config::try_from(new).map_err(|e| SetConfigError::InvalidConfig(e.into()))?;
    let changed = config::set(new).map_err(|e| match e {
        StoreError::Invalid(e) => SetConfigError::InvalidConfig(e.into()),
        StoreError::Append(e) => SetConfigError::Append(e.into()),
    })?;
    // after the write, so a timer reads its new interval; only a moved interval restarts,
    // because a restart pushes the next run a whole interval out
    if changed.expiry {
        task_manager::restart_expiry_timer();
    }
    if changed.audit {
        task_manager::restart_audit_timer();
    }
    if changed.rail_status {
        task_manager::restart_rail_status_timer();
    }
    Ok(())
}
