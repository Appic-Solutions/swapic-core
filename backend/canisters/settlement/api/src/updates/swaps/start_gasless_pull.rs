use crate::types::entry::{PermitSig, PullError};
use crate::types::events::Hash32;

/// Positional, as the endpoint takes them: `(quote_hash, permit)`.
pub type Args = (Hash32, PermitSig);
/// The hash of the pull transaction that was signed and queued.
pub type Response = Result<Hash32, PullError>;
