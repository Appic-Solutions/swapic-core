use crate::types::chain_data::ChainData;
use crate::types::errors::PushChainDataError;

/// Positional, as the endpoint takes them: `(chain_id, data)`.
pub type Args = (u64, ChainData);
pub type Response = Result<(), PushChainDataError>;
