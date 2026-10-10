use crate::ecdsa;
use crate::guards::require_controller;
use ic_cdk::update;
pub use settlement_api::types::errors::SignError;

/// Controller-only. Derives the canister's EVM address from its threshold key and caches
/// it, or answers the cached one. Deploy needs this before anything else: the vaults are
/// configured with this address and it has to be funded for gas, and neither can wait for
/// the first swap to derive it.
#[update]
pub async fn derive_evm_address() -> Result<String, SignError> {
    require_controller().map_err(SignError::Guard)?;
    ecdsa::canister_address()
        .await
        .map(|address| address.to_string())
        .map_err(|e| SignError::Ecdsa(e.into()))
}
