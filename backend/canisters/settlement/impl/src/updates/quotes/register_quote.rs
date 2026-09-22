use crate::guards;
use crate::state::pending_quotes;
use crate::storage::chain_data;
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
    // the height the watcher last reported on the quote's source chain: the deposit that
    // pays this quote lands at or above it, so a claim's log read starts there rather than
    // a day of blocks back. However old the reading is, it only widens that read, never
    // narrows it past the deposit, so its age is not checked here.
    let registered_at = chain_data::get(quote.src_chain).map(|data| data.block);
    let hash = pending_quotes::register(quote, now, registered_at)?;
    Ok(hash.into_bytes())
}
