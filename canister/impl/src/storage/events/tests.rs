use super::*;
use crate::state::transitions::tests::{relay_closed, relay_created, relay_key, relayed};
use crate::state::MemoryStore;
use crate::storage::on_fresh_memory;
use types::events::{TxOutcome, TxPurpose};
use types::LedgerMeta;
use types::{BlockNumber, FlowStatus, TokenAmount, TxHash};

fn fold_both(payloads: Vec<EventType>) -> (State<StableStore>, State<MemoryStore>) {
    let mut stable = State::new(StableStore(()));
    let mut heap = State::<MemoryStore>::default();
    for (i, payload) in (0..).zip(payloads) {
        stable.check(&payload).expect("guard admits");
        let meta = stable.meta();
        let event = Event {
            index: meta.next_event_index,
            timestamp: Timestamp::from_nanos(i),
            payload,
        };
        apply_state_transition(&mut stable, &event);
        apply_state_transition(&mut heap, &event);
    }
    (stable, heap)
}

/// One copy of the transition rules serves both stores, so the stable fold and the heap
/// fold of the same events are the same fold.
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

/// A refused append writes nothing: no log entry, no moved fold.
#[test]
fn a_refused_append_writes_nothing() {
    on_fresh_memory(|| {
        // a signed record spending a number nothing handed out
        assert!(append_event_at(relayed(1, 0), Timestamp::from_nanos(1)).is_err());
        assert_eq!(event_count(), 0);
        assert_eq!(ensure_fold_in_step(), Ok(()));
        append(relay_created(1, 0));
        assert_eq!(event_count(), 1, "and an admissible line still lands");
    });
}

/// Every line is written at the next index with the time it was given, and the log is the
/// fold's: a replay of it is the stable fold.
#[test]
fn the_log_is_its_fold_and_counts_from_zero() {
    on_fresh_memory(|| {
        append(relay_created(1, 0));
        append(relayed(1, 0));
        append(relay_closed(1, true));
        let page = events_page(0, 10);
        assert_eq!(
            page.iter()
                .map(|event| event.index.get())
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert!(page
            .iter()
            .all(|event| event.timestamp == Timestamp::from_nanos(1)));
        assert!(verify_replay());
    });
}

/// A final transaction is a line about a nonce the canister handed out, and moves nothing in
/// the fold but the log's own index.
#[test]
fn a_final_transaction_names_an_allocated_nonce() {
    on_fresh_memory(|| {
        let finalized = |nonce: u64| EventType::TxFinal {
            purpose: TxPurpose::Relay(relay_key(1)),
            chain_id: ChainId::BASE,
            nonce: types::Nonce::new(nonce),
            tx_hash: TxHash::new([1; 32]),
            outcome: TxOutcome::Succeeded {
                block: BlockNumber::new(9),
            },
        };
        assert!(append_event_at(finalized(0), Timestamp::from_nanos(1)).is_err());
        append(relay_created(1, 0));
        append(relayed(1, 0));
        append(finalized(0));
        assert_eq!(event_count(), 3);
        assert!(verify_replay());
    });
}

/// The O(1) rule `post_upgrade` refuses an upgrade on and `append_event` refuses to write
/// on: the fold writes next at the log's length.
#[test]
fn the_fold_check_passes_a_fold_in_step_and_names_what_is_not() {
    on_fresh_memory(|| {
        assert_eq!(ensure_fold_in_step(), Ok(()), "genesis: no events");
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
            Err(FoldOutOfStep {
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
        assert_eq!(event_count(), 2, "a refused append writes nothing");
    });
}
