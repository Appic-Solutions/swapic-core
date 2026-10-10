use crate::types::errors::SignError;
use crate::types::events::Hash32;
use crate::types::evm::EcdsaSignature;

pub type Args = Hash32;
pub type Response = Result<EcdsaSignature, SignError>;
