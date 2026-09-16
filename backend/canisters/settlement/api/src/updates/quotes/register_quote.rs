use crate::types::errors::RegisterQuoteError;
use crate::types::events::Hash32;
use crate::types::quote::Quote;

pub type Args = Quote;
pub type Response = Result<Hash32, RegisterQuoteError>;
