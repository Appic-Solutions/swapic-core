use crate::types::entry::ClaimError;
use crate::types::events::Hash32;
use crate::types::quote::Quote;

pub type Args = Quote;
/// The swap id the deposit was claimed for.
pub type Response = Result<Hash32, ClaimError>;
