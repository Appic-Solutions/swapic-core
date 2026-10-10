use crate::types::errors::GuardError;
use crate::types::events::AuditProgress;

/// The most entries this step folds.
pub type Args = u64;
pub type Response = Result<AuditProgress, GuardError>;
