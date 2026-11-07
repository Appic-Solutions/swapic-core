use crate::guards::require_watcher;
use ic_cdk::query;

/// The canister's cycles balance, for the watcher's low cycles alert alone: a balance anyone
/// could read would tell an outsider how far the canister is from freezing. Any other caller is
/// refused. The balance saturates at `u64::MAX` (about 18 million T), far above any real one.
#[query]
pub fn watcher_cycles_balance() -> Result<u64, String> {
    require_watcher().map_err(|refused| format!("{refused:?}"))?;
    Ok(u64::try_from(ic_cdk::api::canister_balance128()).unwrap_or(u64::MAX))
}
