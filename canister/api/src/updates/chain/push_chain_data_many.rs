use crate::types::chain_data::ChainPush;
use crate::types::errors::PushChainDataError;

pub type Args = Vec<ChainPush>;
/// One result per entry, in order, each `push_chain_data`'s.
pub type Response = Vec<Result<(), PushChainDataError>>;
