use crate::types::events::Hash32;
use crate::types::tx::TxError;

/// Positional, as the endpoint takes them: `(quote_hash, chain_id, to, gas_limit)`.
pub type Args = (Hash32, u64, String, u64);
pub type Response = Result<Hash32, TxError>;
