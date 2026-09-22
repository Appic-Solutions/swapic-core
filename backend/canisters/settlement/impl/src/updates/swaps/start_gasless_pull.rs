use crate::entry;
use ic_cdk::update;
pub use settlement_api::types::entry::{PermitSig, PullError, PullPermit};
pub use settlement_api::types::events::Hash32;
use types::QuoteHash;

/// Quoter-only. Pulls a gasless user's funds into the source chain's vault with the permit
/// they signed, for a quote that is pending, gasless, still payable, and whose token and
/// amount the permit names, and answers the hash of the transaction that does it. The pull
/// creates no swap: the deposit it makes is what `claim_swap` then verifies. Halted, the
/// canister pulls nothing.
#[update]
pub async fn start_gasless_pull(
    quote_hash: Hash32,
    permit: PermitSig,
) -> Result<Hash32, PullError> {
    let permit = PullPermit::try_from(permit).map_err(PullError::Permit)?;
    entry::start_gasless_pull(QuoteHash::new(quote_hash), permit)
        .await
        .map(|tx_hash| tx_hash.into_bytes())
        .map_err(PullError::from)
}
