use crate::guards::require_controller;
use crate::storage::events;
pub use settlement_api::types::errors::GuardError;

/// Test-only, controller-only: flips one bit of the fold's chain head and leaves the log
/// alone, the divergence the head check and the replay audit exist for. Calling it twice
/// puts the head back.
#[ic_cdk::update]
pub fn test_skew_state() -> Result<(), GuardError> {
    require_controller()?;
    events::test_skew_chain_head();
    Ok(())
}
