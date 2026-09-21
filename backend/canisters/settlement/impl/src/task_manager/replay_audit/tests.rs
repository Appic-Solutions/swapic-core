use super::*;
use crate::guards::require_not_halted;
use crate::state::transitions::tests::funds;
use crate::storage::halt::is_halted;
use crate::storage::on_fresh_memory;
use types::config::AuditChunk;
use types::Timestamp;

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

fn set_chunk(events: u32) {
    config::test_set(types::Config {
        audit_chunk_events: AuditChunk::new(events),
        ..config::get()
    });
}

/// Funds `count` swaps, one event each.
fn log_of(count: u64) {
    for nonce in 1..=count {
        events::append_event_at(funds(nonce), Timestamp::from_nanos(nonce))
            .expect("the fold admits it");
    }
}

fn next_index() -> u64 {
    audit_cursor::get().next_index.get()
}

/// The chain is verified a chunk at a time, so a pass costs the same whatever the log holds,
/// and reaching the head starts the next pass at genesis: the whole chain is re-verified on
/// a rolling basis rather than once.
#[test]
fn the_cursor_advances_across_ticks_and_wraps_at_the_head() {
    on_fresh_memory(|| {
        set_chunk(2);
        log_of(5);
        assert_eq!(audit_cursor::get(), AuditCursor::GENESIS);

        run_replay_audit();
        assert_eq!(next_index(), 2);
        run_replay_audit();
        assert_eq!(next_index(), 4);
        run_replay_audit();
        assert_eq!(
            audit_cursor::get(),
            AuditCursor::GENESIS,
            "the head sends the cursor back to the start"
        );
        assert!(!is_halted(), "a sound chain halts nothing");

        // and around again
        run_replay_audit();
        assert_eq!(next_index(), 2);
    });
}

/// The deep check over the whole log: a healthy canister compares equal, and the page says
/// what it did.
#[test]
fn the_paged_replay_compares_a_whole_log_and_finds_it_sound() {
    on_fresh_memory(|| {
        log_of(3);
        let replay = run_audit_replay(0, events::event_count());
        assert_eq!(
            replay,
            AuditReplay {
                window: ReplayWindow {
                    start: 0,
                    folded: 3,
                    log_len: 3,
                    compared: true,
                    matches: true,
                    refused: None,
                },
                halted: false
            }
        );
        assert!(!is_halted());
    });
}

/// A window that does not reach the head folds what it was given and compares nothing, and
/// a window that does not start at genesis cannot even fold: the state it would need was
/// built by the entries before it. Neither condemns the canister.
#[test]
fn a_partial_window_reports_what_it_did_and_halts_nothing() {
    on_fresh_memory(|| {
        log_of(4);
        let head_of_log = run_audit_replay(0, 2).window;
        assert_eq!((head_of_log.folded, head_of_log.log_len), (2, 4));
        assert!(
            !head_of_log.compared && !head_of_log.matches,
            "a window short of the head is measured against nothing"
        );
        assert_eq!(head_of_log.refused, None);
        assert!(!is_halted());

        let mid_log = run_audit_replay(2, 2).window;
        assert_eq!(mid_log.start, 2);
        assert!(!mid_log.compared);
        assert!(
            mid_log.refused.is_some(),
            "a fold has no state to start a later window from"
        );
        assert!(
            !is_halted(),
            "and a window that cannot compare cannot condemn"
        );

        // a window past the end is empty, and empty is not a divergence
        let past_the_end = run_audit_replay(99, 10).window;
        assert_eq!((past_the_end.folded, past_the_end.compared), (0, false));
        assert!(!is_halted());
    });
}
