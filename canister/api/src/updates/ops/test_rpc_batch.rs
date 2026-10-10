use crate::types::errors::TestRpcError;

/// Positional, as the endpoint takes them: `(chain_id, methods, max_bytes)`.
pub type Args = (u64, Vec<String>, u64);
pub type Response = Result<Vec<String>, TestRpcError>;
