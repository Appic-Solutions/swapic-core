use crate::types::errors::SetRolesError;
use candid::Principal;

/// Positional, as the endpoint takes them: `(quoter, watcher)`.
pub type Args = (Principal, Principal);
pub type Response = Result<(), SetRolesError>;
