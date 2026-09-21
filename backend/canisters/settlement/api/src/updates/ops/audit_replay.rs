use crate::types::errors::GuardError;
use crate::types::events::AuditPage;

/// Positional, as the endpoint takes them: `(start, len)`.
pub type Args = (u64, u64);
pub type Response = Result<AuditPage, GuardError>;
