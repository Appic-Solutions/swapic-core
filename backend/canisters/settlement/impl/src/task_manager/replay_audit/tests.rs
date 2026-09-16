use super::*;
use crate::guards::require_not_halted;
use crate::storage::halt::is_halted;

/// The halt is one-way by design: a later clean audit must not clear it, because the
/// canister may have been halted for a reason the audit no longer sees.
#[test]
fn an_audit_failure_halts_and_a_later_pass_does_not_clear_it() {
    set_halted(false);
    record_audit(true);
    assert!(!is_halted(), "a clean audit halts nothing");

    record_audit(false);
    assert!(is_halted());
    assert!(require_not_halted().is_err());

    record_audit(true);
    assert!(is_halted(), "only a human clears a halt");

    set_halted(false);
    assert!(!is_halted());
    assert!(require_not_halted().is_ok());
}

/// An empty log folds to the default state, so the audit passes and halts nothing.
#[test]
fn a_clean_replay_audit_leaves_the_canister_running() {
    set_halted(false);
    run_replay_audit();
    assert!(!is_halted());
}
