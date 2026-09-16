use crate::guards;
use crate::state::pending_quotes;
use ic_cdk::update;
pub use settlement_api::types::events::Hash32;
pub use settlement_api::types::quote::Quote;
use types::Timestamp;

/// Quoter-only, and it takes a quote of either gas mode. Pre-money: it records a quote
/// the quoter has just handed a user so the funds that arrive later can be matched to it,
/// and returns the hash the user's deposit must carry. Nothing of value moves here, so
/// nothing is written to the log.
#[update]
pub fn register_quote(quote: Quote) -> Result<Hash32, String> {
    guards::require_quoter()?;
    let quote = types::Quote::try_from(quote).map_err(|e| e.to_string())?;
    // deliberately no `gas_mode` gate: the store is a hash-to-quote lookup and the mode
    // only starts to matter when funds arrive. Do not add one.
    let now = Timestamp::from_nanos(ic_cdk::api::time()).as_secs();
    pending_quotes::register(quote, now)
        .map(|hash| hash.into_bytes())
        .map_err(|e| e.to_string())
}
