use crate::ecdsa;
use crate::guards::require_controller;
pub use settlement_api::types::errors::SignError;
pub use settlement_api::types::events::Hash32;
pub use settlement_api::types::evm::EcdsaSignature;
use types::TxHash;

/// Test-only, controller-only door onto one threshold signature, so the integration suite
/// can recover it against the canister's own address without a transaction to carry it.
#[ic_cdk::update]
pub async fn test_sign(hash: Hash32) -> Result<EcdsaSignature, SignError> {
    require_controller().map_err(SignError::Guard)?;
    ecdsa::sign(TxHash::new(hash))
        .await
        .map(|signature| EcdsaSignature {
            r: *signature.r(),
            s: *signature.s(),
            y_parity: signature.y_parity(),
        })
        .map_err(|e| SignError::Ecdsa(e.into()))
}
