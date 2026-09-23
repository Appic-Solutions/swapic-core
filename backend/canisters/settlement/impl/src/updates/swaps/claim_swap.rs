use crate::entry;
use ic_cdk::update;
pub use settlement_api::types::entry::ClaimError;
pub use settlement_api::types::events::Hash32;
pub use settlement_api::types::quote::Quote;

/// Quoter, watcher or controller, and the only creator of swaps. Recomputes the quote's
/// id, reads the source chain's vault for the deposit made against it, and once that
/// deposit is deep enough appends `FundsReceived`, which is the swap. Money-first: the swap
/// not existing, the claim asked in time (through the quote's expiry, the permit window and
/// the `claim_grace_s` after it), and nobody it pays to being sanctioned are all refused
/// before an outcall is bought; a marker refuses a second claim for the same quote while
/// the reads are out; and a deposit that is not the quote's token and amount, one whose
/// block was made after the expiry plus the permit window, or a sanctioned payer, is
/// refused after them. A refusal stores nothing. Halted, the canister claims nothing.
///
/// A deposit counts only from the wallet the quote names: on an EVM source chain the
/// quote's `refund_address` IS the paying wallet. The read asks the provider for the
/// vault's `Deposited` logs whose indexed payer (`topics[3]`) is that address, and refuses
/// any other it is handed, so a deposit from any other wallet is not the quote's deposit
/// (it answers `NotFound`), and nobody else's dust under the quote's public hash reaches
/// the read.
#[update]
pub async fn claim_swap(quote: Quote) -> Result<Hash32, ClaimError> {
    let quote = types::Quote::try_from(quote).map_err(|e| ClaimError::InvalidQuote(e.into()))?;
    entry::claim_swap(quote)
        .await
        .map(|quote_hash| quote_hash.into_bytes())
        .map_err(ClaimError::from)
}
