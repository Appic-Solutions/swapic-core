use crate::guards::require_controller;
use crate::tx;
pub use settlement_api::types::events::Hash32;
pub use settlement_api::types::tx::TxError;
use types::config::GasAction;
use types::events::TxPurpose;
use types::{ChainId, GasAmount, QuoteHash, Wei};

/// Test-only, controller-only door onto [`tx::create_and_send`], so the integration suite
/// can drive the nonce allocator and the outbox without the engine that will drive them.
/// Sends a relay keyed by `quote_hash` with a fixed calldata and no value: its signed record
/// creates the relay's flow, and its receipt closes it.
#[ic_cdk::update]
pub async fn test_send(
    quote_hash: Hash32,
    chain_id: u64,
    to: String,
    gas_limit: u64,
) -> Result<Hash32, TxError> {
    require_controller().map_err(TxError::Guard)?;
    // the door's own refusal: a mistyped address in a test is not a refusal of the signing
    // path
    let parsed =
        to.parse().map_err(
            |reason: types::evm::EvmAddressError| TxError::NotAnAddress {
                to: to.clone(),
                reason: reason.into(),
            },
        )?;
    let chain = ChainId::new(chain_id);
    crate::gas::ensure_fresh(chain)
        .await
        .map_err(|_| TxError::StaleChainData { chain_id })?;
    tx::create_and_send(
        TxPurpose::Relay(QuoteHash::new(quote_hash)),
        chain,
        parsed,
        Wei::ZERO,
        vec![0xde, 0xad, 0xbe, 0xef],
        GasAmount::from(gas_limit),
        GasAction::RelayLocal,
    )
    .await
    .map(|signed| signed.tx_hash.into_bytes())
    .map_err(TxError::from)
}
