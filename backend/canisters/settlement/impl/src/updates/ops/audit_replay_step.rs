use crate::guards::require_controller;
use crate::task_manager::replay_audit::{self, AuditProgress as Progress};
use ic_cdk::update;
pub use settlement_api::types::errors::GuardError;
pub use settlement_api::types::events::AuditProgress;

impl From<Progress> for AuditProgress {
    fn from(progress: Progress) -> Self {
        let Progress {
            folded_so_far,
            remaining,
            finished,
            matches,
            refused,
            halted,
        } = progress;
        Self {
            folded_so_far,
            remaining,
            finished,
            matches,
            refused: refused.map(Into::into),
            halted,
        }
    }
}

/// Controller-only. One step of the deep check: folds up to `max_events` more of the log
/// onto the fold the step before saved, from genesis when there is none, and once the fold
/// reaches the head compares it with the live state. An update, not a query, because a
/// divergence it finds halts the canister, and because the fold so far is saved in stable
/// memory between steps, so an audit spans as many calls as the log needs and survives an
/// upgrade in the middle.
///
/// Each step answers how much is folded and how much is left; keep calling until
/// `finished`. A refusal, a broken link, or a finished fold that differs from the live
/// state halts the canister, and every verdict starts the next audit over from genesis.
///
/// A step costs the entries it folds plus the fold so far, which is read and written whole,
/// so on a large fold the fold's own size is the floor and `max_events` is the rest. A step
/// too large for one message fails that call alone and leaves the saved fold where it was.
#[update]
pub fn audit_replay_step(max_events: u64) -> Result<AuditProgress, GuardError> {
    require_controller()?;
    Ok(replay_audit::run_audit_replay_step(max_events).into())
}
