//! EVM to ICP: the ckUSDC that lands in a registered quote's subaccount is swapped on
//! ICPSwap and paid to the user's principal.

use super::flows::FlowDriveError;
use types::{Flow, QuoteHash};

/// The flow's next step.
pub async fn step(_quote_hash: QuoteHash, _flow: &Flow) -> Result<(), FlowDriveError> {
    Ok(())
}
