use crate::entry;
use ic_cdk::update;
pub use settlement_api::types::entry::ClaimError;
pub use settlement_api::types::events::Hash32;
pub use settlement_api::types::quote::Quote;

/// Quoter or watcher, and the only creator of swaps. Recomputes the quote's id, reads the
/// source chain's vault for the deposit made against it, and once that deposit is deep
/// enough appends `FundsReceived`, which is the swap. Money-first: the swap not existing,
/// the quote still claimable (through its expiry plus the permit window), and nobody it
/// pays to being sanctioned are all refused before an outcall is bought; a marker refuses a
/// second claim for the same quote while the read is out; and a deposit that is not the
/// quote's token and amount, or a sanctioned payer, is refused after it. A refusal stores
/// nothing. Halted, the canister claims nothing.
#[update]
pub async fn claim_swap(quote: Quote) -> Result<Hash32, ClaimError> {
    let quote = types::Quote::try_from(quote).map_err(|e| ClaimError::InvalidQuote(e.into()))?;
    entry::claim_swap(quote)
        .await
        .map(|quote_hash| quote_hash.into_bytes())
        .map_err(ClaimError::from)
}
