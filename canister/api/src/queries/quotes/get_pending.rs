use crate::types::errors::GuardError;
use crate::types::events::Hash32;
use crate::types::quote::Quote;

pub type Args = Hash32;
pub type Response = Result<Option<Quote>, GuardError>;
