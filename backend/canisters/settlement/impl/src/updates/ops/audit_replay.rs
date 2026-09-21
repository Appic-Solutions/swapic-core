use crate::guards::require_controller;
use crate::storage::events::ReplayWindow;
use crate::task_manager::replay_audit::{self, AuditReplay};
use ic_cdk::update;
pub use settlement_api::types::errors::GuardError;
pub use settlement_api::types::events::AuditPage;

impl From<AuditReplay> for AuditPage {
    fn from(replay: AuditReplay) -> Self {
        let AuditReplay { window, halted } = replay;
        let ReplayWindow {
            start,
            folded,
            log_len,
            compared,
            matches,
            refused,
        } = window;
        Self {
            start,
            folded,
            log_len,
            compared,
            matches,
            refused: refused.map(Into::into),
            halted,
        }
    }
}

/// Controller-only. The deep check by hand: folds the log window `[start, start + len)` and,
/// where the window is the whole log, compares it with the live state. An update, not a
/// query, because a divergence it finds halts the canister.
///
/// Only a window from index 0 that reaches the head can be compared: a fold has no state to
/// start a later window from. Ask for the count first and spend as many calls as the log
/// needs.
#[update]
pub fn audit_replay(start: u64, len: u64) -> Result<AuditPage, GuardError> {
    require_controller()?;
    Ok(replay_audit::run_audit_replay(start, len).into())
}
