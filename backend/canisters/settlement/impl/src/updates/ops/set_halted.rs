use crate::guards::require_controller;
use crate::storage::halt;
use ic_cdk::update;
pub use settlement_api::types::errors::GuardError;

/// Controller-only, both ways: `true` is an emergency stop, `false` is the human saying
/// the divergence has been investigated. The audit never clears it on its own.
#[update]
pub fn set_halted(halted: bool) -> Result<(), GuardError> {
    require_controller()?;
    halt::set_halted(halted);
    Ok(())
}
