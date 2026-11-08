use crate::guards::require_watcher_or_controller;
use ic_cdk::query;
pub use settlement_api::types::pool::PullToCheck;

/// The doors' pulls whose outcome nobody could tell, oldest first, for the watcher's alert and
/// a controller's check (audit C15: nothing is written of a pull before its money is in). Any
/// other caller is refused.
#[query]
pub fn pulls_to_check() -> Result<Vec<PullToCheck>, String> {
    require_watcher_or_controller().map_err(|refused| format!("{refused:?}"))?;
    Ok(crate::pool::pull_first::pulls_to_check()
        .into_iter()
        .map(PullToCheck::from)
        .collect())
}
