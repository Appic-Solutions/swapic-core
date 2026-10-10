use crate::guards;
use crate::state::pending_quotes;
use crate::storage::{chain_data, config};
use ic_cdk::update;
pub use settlement_api::types::errors::RegisterQuoteError;
pub use settlement_api::types::events::Hash32;
pub use settlement_api::types::quote::Quote;
use types::Timestamp;

/// Quoter-only, for a quote of either gas mode. Records a quote the quoter handed a user,
/// so the funds that arrive later can be matched to it, and returns the hash the deposit
/// must carry. Pre-money, so nothing is written to the log. Refuses what a claim would
/// refuse by the quote alone: a rail the deploy has off, a payee that is no address or the
/// zero address, and a token on either side that is not the USDC the deploy configured for
/// that chain (`RailToken`), which the claim refuses on the same config. Halted, the
/// canister registers nothing: no new swaps during a halt. A full store first drops quotes
/// whose claim's grace has ended. The quote's deposit and claim deadlines are fixed here,
/// from the permit window and the grace the config holds now, and a later config change
/// moves neither.
#[update]
pub fn register_quote(quote: Quote) -> Result<Hash32, RegisterQuoteError> {
    // the halt stops new swaps at their first door, as the claim's own guard does
    guards::require_not_halted().map_err(RegisterQuoteError::Guard)?;
    guards::require_quoter().map_err(RegisterQuoteError::Guard)?;
    let quote =
        types::Quote::try_from(quote).map_err(|e| RegisterQuoteError::InvalidQuote(e.into()))?;
    // deliberately no `gas_mode` gate: the mode only matters once funds arrive
    let now = Timestamp::from_nanos(ic_cdk::api::time());
    // the head of the quote's source chain, from a reading the canister holds as fresh by
    // the rule every money decision on the cache takes, or no height at all: the deposit
    // that pays this quote lands at or above it, so a claim's log read starts a margin
    // below it rather than a day of blocks back. The reading is the watcher's and can run
    // ahead of the chain, so the claim bounds it by the provider's own head and reads the
    // rest of the lookback when nothing is found above it (see `entry::claim_swap`).
    let registered_at = chain_data::fresh(quote.src_chain, now, config::get().chain_data_max_age)
        .map(|data| data.block);
    let hash = pending_quotes::register(quote, now.as_secs(), registered_at, &config::get())?;
    Ok(hash.into_bytes())
}
