use super::*;
use crate::state::transitions::tests::{relay_closed, relay_created, relay_key, relayed};
use crate::state::transitions::ReplayError;
use crate::state::MemoryStore;
use crate::storage::halt::is_halted;
use crate::storage::on_fresh_memory;
use crate::task_manager::replay_audit::{run_audit_replay_step, run_replay_audit};
use types::LedgerMeta;
use types::{FlowStatus, FlowStep, TokenAmount, TxHash};

fn fold_both(payloads: Vec<EventType>) -> (State<StableStore>, State<MemoryStore>) {
    let mut stable = State::new(StableStore(()));
    let mut heap = State::<MemoryStore>::default();
    for (i, payload) in (0..).zip(payloads) {
        stable.check(&payload).expect("guard admits");
        let meta = stable.meta();
        let event = Event::seal(
            meta.next_event_index,
            Timestamp::from_nanos(i),
            meta.last_event_hash,
            payload,
        )
        .unwrap();
        apply_state_transition(&mut stable, &event);
        apply_state_transition(&mut heap, &event);
    }
    (stable, heap)
}

/// One copy of the transition rules serves both stores, so the stable fold and the heap
/// fold of the same events are the same fold: what the replay audit relies on.
#[test]
fn the_stable_fold_matches_the_heap_fold() {
    let (stable, heap) = fold_both(vec![
        relay_created(1, 0),
        relayed(1, 0),
        relay_created(2, 1),
        EventType::Books {
            chain_id: ChainId::BASE,
            step: types::books::BookStep::FeeEarned {
                tx_hash: TxHash::new([1; 32]),
                amount: TokenAmount::from(400_u32),
                block: None,
            },
        },
        relay_closed(1, true),
    ]);
    assert!(heap.matches(&stable));
    assert_eq!(stable.meta().next_event_index, EventIndex::new(5));
    assert_eq!(
        stable.store().flow(&relay_key(1)).unwrap().status,
        FlowStatus::RelayConfirmed
    );
    assert_eq!(
        stable.unsigned_nonces().len(),
        1,
        "relay 2 is still being signed"
    );
}

fn append(payload: EventType) {
    append_event_at(payload, Timestamp::from_nanos(1)).expect("the stable fold admits it");
}

fn log_replay() -> Result<State<MemoryStore>, ReplayError> {
    EVENTS.with(|e| replay(e.borrow().iter()))
}

/// The deep check ops run by hand, in one step the size of the log: the comparison
/// a bounded timer pass cannot make. Answers whether it halted the canister.
fn deep_check_halts() -> bool {
    let step = run_audit_replay_step(event_count());
    assert!(
        step.finished || step.halted,
        "one step the size of the log ends the audit: {step:?}"
    );
    assert_eq!(step.halted, is_halted(), "a halting step sets the flag");
    step.halted
}

/// A refused append writes nothing: no log entry, no moved fold. A retired v1 line is one.
#[test]
fn a_refused_append_writes_nothing() {
    on_fresh_memory(|| {
        let swap = QuoteHash::new([5; 32]);
        assert_eq!(
            append_event_at(
                EventType::SwapDone { quote_hash: swap },
                Timestamp::from_nanos(1)
            ),
            Err(AppendError::Transition(TransitionError::UnknownSwap(swap)))
        );
        assert_eq!(event_count(), 0);
        assert_eq!(ensure_fold_in_step(), Ok(()));

        append(relay_created(1, 0));
        assert_eq!(event_count(), 1, "and an admissible line still lands");
    });
}

/// The cheap invariant runs every tick: a fold whose head left the log's is exactly what
/// `append_event` refuses to write on, so the audit halts on it without folding anything.
#[test]
fn a_fold_out_of_step_with_its_log_halts_the_next_pass() {
    on_fresh_memory(|| {
        append(relay_created(1, 0));
        run_replay_audit();
        assert!(!is_halted());

        let meta = StableStore(()).meta();
        StableStore(()).put_meta(LedgerMeta {
            last_event_hash: EventHash::new([7; 32]),
            ..meta
        });
        run_replay_audit();
        assert!(is_halted(), "the head check halts the tick");
    });
}

/// A flow sits in the stable fold that no event created, and the log then builds on it.
/// The heap replay refuses the first event on it, so the audit answers false and halts,
/// where it would trap on the fold's own reads before it could halt.
#[test]
fn replay_audit_halts_on_a_flow_no_event_created() {
    on_fresh_memory(|| {
        let ghost = types::Flow::apply(
            None,
            &FlowStep::GaslessRelayed {
                chain_id: ChainId::BASE,
                nonce: Nonce::ZERO,
                tx_hash: TxHash::new([1; 32]),
                raw_tx: vec![],
            },
            0,
            Timestamp::from_nanos(1),
        );
        StableStore(()).put_flow(relay_key(1), ghost);
        // the stable fold holds the ghost, so it admits its receipt; the replay of the log,
        // which never signed the relay, does not
        append(relay_closed(1, true));

        assert!(verify_chain(), "the chain itself is sound");
        assert_eq!(
            log_replay().unwrap_err(),
            ReplayError::Refused {
                index: EventIndex::ZERO,
                error: types::FlowError::Unknown(relay_key(1)).into()
            }
        );
        assert!(!verify_replay());
        run_replay_audit();
        assert!(
            !is_halted(),
            "a bounded pass checks the chain and the head, not the whole fold"
        );
        assert!(
            deep_check_halts(),
            "the deep check halts instead of trapping"
        );
    });
}

/// An allocator count sits in the stable fold that no event produced, and the log spends
/// the number after it.
#[test]
fn replay_audit_halts_on_a_nonce_no_event_allocated() {
    on_fresh_memory(|| {
        StableStore(()).put_next_nonce(ChainId::BASE, Nonce::new(1));
        append(relay_created(1, 1));

        assert_eq!(
            log_replay().unwrap_err(),
            ReplayError::Refused {
                index: EventIndex::ZERO,
                error: TransitionError::NonceOutOfSequence {
                    chain_id: ChainId::BASE,
                    nonce: Nonce::new(1),
                    expected: Nonce::ZERO,
                }
            }
        );
        assert!(!verify_replay());
        run_replay_audit();
        assert!(
            !is_halted(),
            "a bounded pass checks the chain and the head, not the whole fold"
        );
        assert!(deep_check_halts());
    });
}

/// A divergence the log folds cleanly over is still a divergence: the comparison catches
/// what the replay admits.
#[test]
fn replay_audit_halts_on_a_fold_that_replays_cleanly_but_differs() {
    on_fresh_memory(|| {
        append(relay_created(1, 0));
        append(relayed(1, 0));
        let mut flow = StableStore(()).flow(&relay_key(1)).unwrap();
        flow.status = FlowStatus::RelayFailed;
        StableStore(()).put_flow(relay_key(1), flow);

        assert!(log_replay().is_ok(), "every event is admissible");
        assert!(!verify_replay());
        run_replay_audit();
        assert!(
            !is_halted(),
            "a bounded pass checks the chain and the head, not the whole fold"
        );
        assert!(deep_check_halts());
    });
}

/// The O(1) rule `post_upgrade` refuses an upgrade on and `append_event` refuses to write
/// on: the fold seals next at the log's length, linked to the log's last hash.
#[test]
fn the_fold_check_passes_a_fold_in_step_and_names_what_is_not() {
    on_fresh_memory(|| {
        assert_eq!(
            ensure_fold_in_step(),
            Ok(()),
            "genesis: no events, zero head"
        );
        let zero = StableStore(()).meta();
        StableStore(()).put_meta(LedgerMeta {
            last_event_hash: EventHash::new([7; 32]),
            ..zero
        });
        assert_eq!(
            ensure_fold_in_step(),
            Err(FoldOutOfStep::Head {
                log: EventHash::ZERO,
                fold: EventHash::new([7; 32])
            }),
            "an empty log ends with the zero hash"
        );
        StableStore(()).put_meta(zero);

        append(relay_created(1, 0));
        append(relay_created(2, 1));
        assert_eq!(ensure_fold_in_step(), Ok(()));
        let meta = StableStore(()).meta();

        StableStore(()).put_meta(LedgerMeta {
            next_event_index: EventIndex::new(3),
            ..meta
        });
        assert_eq!(
            ensure_fold_in_step(),
            Err(FoldOutOfStep::Length {
                log_len: 2,
                next_event_index: EventIndex::new(3)
            })
        );
        assert_eq!(
            append_event_at(relay_created(3, 2), Timestamp::from_nanos(1)),
            Err(AppendError::IndexMismatch {
                log_len: 2,
                sealed: EventIndex::new(3)
            })
        );

        StableStore(()).put_meta(LedgerMeta {
            last_event_hash: EventHash::new([7; 32]),
            ..meta
        });
        assert_eq!(
            ensure_fold_in_step(),
            Err(FoldOutOfStep::Head {
                log: meta.last_event_hash,
                fold: EventHash::new([7; 32])
            })
        );
        assert!(matches!(
            append_event_at(relay_created(3, 2), Timestamp::from_nanos(1)),
            Err(AppendError::ChainDiverged { .. })
        ));
        assert_eq!(event_count(), 2, "a refused append writes nothing");
    });
}
