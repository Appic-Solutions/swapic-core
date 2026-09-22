use crate::types::entry::{EcoIntent, PushEcoIntentError};
use crate::types::events::Hash32;

/// Positional, as the endpoint takes them: `(quote_hash, intent)`.
pub type Args = (Hash32, EcoIntent);
pub type Response = Result<(), PushEcoIntentError>;
