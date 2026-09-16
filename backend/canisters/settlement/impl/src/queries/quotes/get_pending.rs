use crate::guards;
use crate::state::pending_quotes;
use ic_cdk::query;
pub use settlement_api::types::errors::GuardError;
pub use settlement_api::types::events::Hash32;
pub use settlement_api::types::quote::Quote;
use types::QuoteHash;

/// Quoter or watcher. Not public: a pending quote carries the user's destination and
/// refund addresses.
#[query]
pub fn get_pending(quote_hash: Hash32) -> Result<Option<Quote>, GuardError> {
    guards::require_quoter_or_watcher()?;
    Ok(pending_quotes::get_pending(&QuoteHash::new(quote_hash)).map(Quote::from))
}
