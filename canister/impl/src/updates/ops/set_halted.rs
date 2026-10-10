use crate::guards::require_controller;
use crate::storage::halt;
use crate::task_manager;
use ic_cdk::update;
pub use settlement_api::types::errors::GuardError;

/// Controller-only, both ways: `true` is an emergency stop, `false` is the human saying
/// the divergence has been investigated. The audit never clears it on its own.
///
/// Lifting the halt wakes the outbox: a halted canister's pass does not re-arm itself,
/// so this is what puts it back on the window when a queued transaction or an allocation
/// waiting for its cancel was held back by the halt.
#[update]
pub fn set_halted(halted: bool) -> Result<(), GuardError> {
    require_controller()?;
    halt::set_halted(halted);
    if !halted {
        task_manager::outbox::arm_if_work_is_pending();
    }
    Ok(())
}
