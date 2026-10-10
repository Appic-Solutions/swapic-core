use crate::guards::require_controller;
use crate::rpc;
use serde_json::json;
pub use settlement_api::types::errors::TestRpcError;
use types::ChainId;

/// Test-only, controller-only door onto [`rpc::rpc_batch`], so the integration suite can
/// watch what one batch does to the outcall queue. Every call goes out with empty params,
/// and each result comes back as its JSON text, because this door exists to prove the
/// batching and the ordering rather than to read a chain.
#[ic_cdk::update]
pub async fn test_rpc_batch(
    chain_id: u64,
    methods: Vec<String>,
    max_bytes: u64,
) -> Result<Vec<String>, TestRpcError> {
    require_controller().map_err(TestRpcError::Guard)?;
    let calls: Vec<(&str, serde_json::Value)> = methods
        .iter()
        .map(|method| (method.as_str(), json!([])))
        .collect();
    let results = rpc::rpc_batch(ChainId::new(chain_id), &calls, max_bytes)
        .await
        .map_err(|e| TestRpcError::Rpc(e.into()))?;
    Ok(results.iter().map(|value| value.to_string()).collect())
}
