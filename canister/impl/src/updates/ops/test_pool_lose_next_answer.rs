use crate::guards::require_controller;
use settlement_api::types::errors::GuardError;

/// Test-only, controller-only: the pool's next ICRC transfer is executed by its ledger and
/// its answer lost on the way back, as a subnet that lost it would. The payout asks again
/// with the same dedup pair, which is what this exists to show.
#[ic_cdk::update]
pub fn test_pool_lose_next_answer() -> Result<(), GuardError> {
    require_controller()?;
    crate::pool::payout::lose_next_answer();
    Ok(())
}
