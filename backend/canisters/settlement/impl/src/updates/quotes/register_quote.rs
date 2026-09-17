use crate::guards;
use crate::state::pending_quotes;
use ic_cdk::update;
pub use settlement_api::types::errors::RegisterQuoteError;
pub use settlement_api::types::events::Hash32;
pub use settlement_api::types::quote::Quote;
use types::Timestamp;

/// Quoter-only, for a quote of either gas mode. Records a quote the quoter handed a user,
/// so the funds that arrive later can be matched to it, and returns the hash the deposit
/// must carry. Pre-money, so nothing is written to the log.
#[update]
pub fn register_quote(quote: Quote) -> Result<Hash32, RegisterQuoteError> {
    guards::require_quoter().map_err(RegisterQuoteError::Guard)?;
    let quote =
        types::Quote::try_from(quote).map_err(|e| RegisterQuoteError::InvalidQuote(e.into()))?;
    // deliberately no `gas_mode` gate: the mode only matters once funds arrive
    let now = Timestamp::from_nanos(ic_cdk::api::time()).as_secs();
    let hash = pending_quotes::register(quote, now)?;
    Ok(hash.into_bytes())
}
