use super::*;
use crate::guards::require_not_halted;
use crate::state::transitions::tests::{funds, swap_id};
use crate::state::transitions::ReplayError;
use crate::storage::halt::is_halted;
use crate::storage::on_fresh_memory;
use crate::storage::replay_cursor::{self, ReplayCursor};
use types::config::AuditChunk;
use types::events::EventType;
use types::{Attempt, ChainId, Event, EventIndex, Timestamp, TransitionError, TxHash};

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

/// Funds one swap per nonce in `nonces`, one event each.
fn fund(nonces: impl IntoIterator<Item = u64>) {
    for nonce in nonces {
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
        fund(1..=5);
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

/// What one step reports before it is finished: how far the fold got and what is left.
fn unfinished(step: AuditProgress) -> (u64, u64) {
    assert!(
        !step.finished && !step.matches && step.refused.is_none() && !step.halted,
        "{step:?}"
    );
    (step.folded_so_far, step.remaining)
}

/// The deep check over a log too long to fold in one message: three steps of a thousand
/// fold three thousand events, the third reaches the head and compares, and the verdict is
/// the one an unbounded fold gives. A finished audit starts the next one at genesis.
#[test]
fn the_deep_audit_folds_a_long_log_in_bounded_steps_and_finds_it_sound() {
    on_fresh_memory(|| {
        fund(1..=3_000);

        assert_eq!(unfinished(run_audit_replay_step(1_000)), (1_000, 2_000));
        assert_eq!(unfinished(run_audit_replay_step(1_000)), (2_000, 1_000));
        assert_eq!(
            run_audit_replay_step(1_000),
            AuditProgress {
                folded_so_far: 3_000,
                remaining: 0,
                finished: true,
                matches: true,
                refused: None,
                halted: false,
            }
        );
        assert!(!is_halted());
        assert!(
            events::verify_replay(),
            "the same verdict as one unbounded fold"
        );
        assert_eq!(
            replay_cursor::get(),
            ReplayCursor::genesis(),
            "a finished audit starts the next at genesis"
        );

        // the next audit starts over, and one step the size of the log finishes it
        let whole = run_audit_replay_step(3_000);
        assert_eq!(
            (whole.folded_so_far, whole.finished, whole.matches),
            (3_000, true, true)
        );
    });
}

/// A divergence deep in the log is met by the step that reaches it and by no step before:
/// planted at index 2,500, it halts the third step of a thousand, not the first. A halted
/// audit starts the next one at genesis, and the verdict is the one an unbounded fold gives.
#[test]
fn a_divergence_deep_in_the_log_halts_the_step_that_reaches_it() {
    on_fresh_memory(|| {
        fund(1..=2_500);
        // an entry no guard admitted, on a swap the log never funded, written raw with the
        // fold moved onto it so the log builds on from there
        let meta = events::read_state(|state| state.meta());
        assert_eq!(meta.next_event_index, EventIndex::new(2_500));
        let orphan = swap_id(9_999);
        let planted = Event::seal(
            meta.next_event_index,
            Timestamp::from_nanos(2_501),
            meta.last_event_hash,
            EventType::TxSigned {
                quote_hash: orphan,
                attempt: Attempt::FIRST,
                chain_id: ChainId::BASE,
                tx_hash: TxHash::new([1; 32]),
                raw_tx: vec![],
            },
        )
        .expect("the payload has a preimage");
        events::test_push_raw(planted);
        fund(2_502..=3_000);
        assert_eq!(events::event_count(), 3_000);

        assert_eq!(unfinished(run_audit_replay_step(1_000)), (1_000, 2_000));
        assert!(!is_halted(), "the first two thousand entries are sound");
        assert_eq!(unfinished(run_audit_replay_step(1_000)), (2_000, 1_000));
        assert!(!is_halted());

        assert_eq!(
            run_audit_replay_step(1_000),
            AuditProgress {
                folded_so_far: 2_500,
                remaining: 500,
                finished: false,
                matches: false,
                refused: Some(ReplayError::Refused {
                    index: EventIndex::new(2_500),
                    error: TransitionError::UnknownSwap(orphan),
                }),
                halted: true,
            },
            "the step that reaches the entry halts"
        );
        assert!(is_halted());
        assert_eq!(
            replay_cursor::get(),
            ReplayCursor::genesis(),
            "a halted audit starts the next one over"
        );
        assert!(
            !events::verify_replay(),
            "the same verdict as one unbounded fold"
        );
    });
}

/// The log keeps growing while an audit runs, so a step folds towards the head as it stands
/// when the step reads it, not as it stood when the audit began.
#[test]
fn a_log_that_grows_between_steps_is_audited_to_its_new_head() {
    on_fresh_memory(|| {
        fund(1..=5);
        assert_eq!(unfinished(run_audit_replay_step(3)), (3, 2));
        fund(6..=9);
        let last = run_audit_replay_step(100);
        assert_eq!(
            (
                last.folded_so_far,
                last.remaining,
                last.finished,
                last.matches
            ),
            (9, 0, true, true),
            "{last:?}"
        );
        assert!(!is_halted());
    });
}

/// A step of nothing folds nothing and moves nothing, and on an empty log it is the whole
/// audit: nothing to fold, nothing to differ.
#[test]
fn a_step_of_nothing_is_a_report_and_an_empty_log_audits_at_once() {
    on_fresh_memory(|| {
        let empty = run_audit_replay_step(0);
        assert_eq!(
            (
                empty.folded_so_far,
                empty.finished,
                empty.matches,
                empty.halted
            ),
            (0, true, true, false),
            "{empty:?}"
        );

        fund(1..=2);
        assert_eq!(unfinished(run_audit_replay_step(0)), (0, 2));
        assert_eq!(unfinished(run_audit_replay_step(1)), (1, 1));
        assert_eq!(unfinished(run_audit_replay_step(0)), (1, 1));
        assert!(run_audit_replay_step(1).finished);
    });
}
