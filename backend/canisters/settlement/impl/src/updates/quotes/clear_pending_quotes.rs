use crate::guards::require_controller;
use crate::state::pending_quotes;
use ic_cdk::update;
pub use settlement_api::types::errors::GuardError;

/// Controller-only. Empties the pending store and answers how many quotes it held: the
/// store survives upgrades, so a store a compromised quoter filled is recovered here, in
/// place. Pre-money, so nothing is written to the log; the quoter re-registers what is live.
#[update]
pub fn clear_pending_quotes() -> Result<u64, GuardError> {
    require_controller()?;
    Ok(pending_quotes::clear())
}
