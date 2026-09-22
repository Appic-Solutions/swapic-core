use crate::types::errors::SetSanctionedError;

/// Positional, as the endpoint takes them: `(add, remove)`.
pub type Args = (Vec<String>, Vec<String>);
/// How many addresses the set holds after the call.
pub type Response = Result<u64, SetSanctionedError>;
