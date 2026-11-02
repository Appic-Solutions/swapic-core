use crate::guards::require_controller;
use crate::storage::halt;
use crate::task_manager;
use ic_cdk::update;
pub use settlement_api::types::errors::GuardError;

/// Controller-only, both ways: `true` is an emergency stop, `false` lifts it.
///
/// Lifting the halt wakes the outbox and the engine's backstop: neither re-arms itself on a
/// halted canister, so this is what puts them back when a queued transaction, an allocation
/// waiting for its cancel or an open payout was held back by the halt.
#[update]
pub fn set_halted(halted: bool) -> Result<(), GuardError> {
    require_controller()?;
    halt::set_halted(halted);
    if !halted {
        task_manager::outbox::arm_if_work_is_pending();
        task_manager::backstop::arm_if_open();
    }
    Ok(())
}
