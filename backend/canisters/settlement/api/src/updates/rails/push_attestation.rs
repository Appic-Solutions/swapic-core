use crate::types::entry::PushAttestationError;
use crate::types::events::Hash32;

/// Positional, as the endpoint takes them: `(quote_hash, message, attestation)`.
pub type Args = (Hash32, Vec<u8>, Vec<u8>);
pub type Response = Result<(), PushAttestationError>;
