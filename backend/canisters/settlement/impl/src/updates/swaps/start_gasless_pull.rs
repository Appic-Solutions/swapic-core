use crate::entry;
use ic_cdk::update;
pub use settlement_api::types::entry::{PullError, PullPermit, PullRequest};
pub use settlement_api::types::events::Hash32;
use types::QuoteHash;

/// Quoter-only. Pulls a gasless user's funds into the source chain's vault with the
/// Permit2 permit they signed, for a quote that is pending, gasless and still payable, and
/// answers the hash of the transaction that does it. The permit has to be witnessed by
/// this quote and name its token, its amount and the vault as the spender; an EIP-2612
/// permit is refused, because the vault's 2612 door transfers on a standing allowance
/// whether or not the permit verified and a 2612 signature names no quote. The pull
/// creates no swap: the deposit it makes is what `claim_swap` then verifies. Halted, the
/// canister pulls nothing.
#[update]
pub async fn start_gasless_pull(
    quote_hash: Hash32,
    request: PullRequest,
) -> Result<Hash32, PullError> {
    let permit = PullPermit::try_from(request).map_err(PullError::Permit)?;
    entry::start_gasless_pull(QuoteHash::new(quote_hash), permit)
        .await
        .map(|tx_hash| tx_hash.into_bytes())
        .map_err(PullError::from)
}
