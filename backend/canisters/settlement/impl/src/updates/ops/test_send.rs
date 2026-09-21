use crate::guards::require_controller;
use crate::tx;
pub use settlement_api::types::events::Hash32;
pub use settlement_api::types::tx::TxError;
use types::events::TxPurpose;
use types::{ChainId, GasAmount, QuoteHash, Wei};

/// Test-only, controller-only door onto [`tx::create_and_send`], so the integration suite
/// can drive the nonce allocator and the outbox without the engine that will drive them.
/// Sends a payout for `quote_hash` with a fixed calldata and no value.
#[ic_cdk::update]
pub async fn test_send(
    quote_hash: Hash32,
    chain_id: u64,
    to: String,
    gas_limit: u64,
) -> Result<Hash32, TxError> {
    require_controller().map_err(TxError::Guard)?;
    let to = to.parse().map_err(|_| TxError::PurposeNeedsASwap {
        purpose: format!("{to} is not an address"),
    })?;
    tx::create_and_send(
        TxPurpose::Payout(QuoteHash::new(quote_hash)),
        ChainId::new(chain_id),
        to,
        Wei::ZERO,
        vec![0xde, 0xad, 0xbe, 0xef],
        GasAmount::from(gas_limit),
    )
    .await
    .map(|hash| hash.into_bytes())
    .map_err(TxError::from)
}
