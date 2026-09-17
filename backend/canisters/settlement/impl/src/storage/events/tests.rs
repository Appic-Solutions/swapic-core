use super::*;
use crate::state::transitions::ReplayError;
use crate::state::MemoryStore;
use crate::storage::halt::is_halted;
use crate::storage::on_fresh_memory;
use crate::task_manager::replay_audit::run_replay_audit;
use types::{Attempt, SwapStatus, TokenAmount, TxHash};

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
        );
        apply_state_transition(&mut stable, &event);
        apply_state_transition(&mut heap, &event);
    }
    (stable, heap)
}

/// One copy of the transition rules serves both stores, so the stable fold and the heap
/// fold of the same events are the same fold: what the replay audit relies on.
#[test]
fn the_stable_fold_matches_the_heap_fold() {
    let quote = QuoteHash::new([7; 32]);
    let amount = |value: u32| TokenAmount::from(value);
    let (stable, heap) = fold_both(vec![
        EventType::PocketFunded {
            chain_id: ChainId::BASE,
            amount: amount(1_000),
        },
        EventType::FundsReceived {
            quote_hash: quote,
            quote_bytes: vec![0xde, 0xad],
            chain_id: ChainId::BASE,
            token: "USDC".parse().unwrap(),
            amount: amount(100),
            tx_ref: "0xfeed".into(),
        },
        EventType::PocketReserved {
            quote_hash: quote,
            chain_id: ChainId::BASE,
            amount: amount(400),
        },
        EventType::TxSigned {
            quote_hash: quote,
            attempt: Attempt::FIRST,
            chain_id: ChainId::BASE,
            tx_hash: TxHash::new([1; 32]),
            raw_tx: vec![],
        },
    ]);
    assert!(heap.matches(&stable));
    assert_eq!(stable.meta().next_event_index, EventIndex::new(4));
    assert_eq!(stable.pocket(&ChainId::BASE).unwrap().reserved, amount(400));
    assert_eq!(
        stable.swap(&quote).unwrap().open_attempt,
        Some(Attempt::FIRST)
    );
}

fn funds(quote_hash: QuoteHash) -> EventType {
    EventType::FundsReceived {
        quote_hash,
        quote_bytes: vec![],
        chain_id: ChainId::BASE,
        token: "USDC".parse().unwrap(),
        amount: TokenAmount::from(1_u32),
        tx_ref: "0x".into(),
    }
}

fn append(payload: EventType) {
    append_event_at(payload, Timestamp::from_nanos(1)).expect("the stable fold admits it");
}

fn log_replay() -> Result<State<MemoryStore>, ReplayError> {
    EVENTS.with(|e| replay(e.borrow().iter()))
}

/// A swap sits in the stable fold that no event created, and the log then builds on it.
/// The heap replay refuses the first event on it, so the audit answers false and halts,
/// where it used to trap on the fold's `expect` before it could halt.
#[test]
fn replay_audit_halts_on_a_swap_no_event_created() {
    on_fresh_memory(|| {
        let quote = QuoteHash::new([9; 32]);
        let ghost = Swap {
            quote_bytes: vec![],
            status: SwapStatus::FundsReceived,
            last_attempt: None,
            open_attempt: None,
            src_chain: ChainId::BASE,
            src_token: "USDC".parse().unwrap(),
            amount_in: TokenAmount::from(1_u32),
            amount_paid: TokenAmount::ZERO,
            waiting_since: None,
        };
        StableStore(()).put_swap(quote, ghost);
        append(EventType::TxSigned {
            quote_hash: quote,
            attempt: Attempt::FIRST,
            chain_id: ChainId::BASE,
            tx_hash: TxHash::new([1; 32]),
            raw_tx: vec![],
        });

        assert!(verify_chain(), "the chain itself is sound");
        assert_eq!(
            log_replay().unwrap_err(),
            ReplayError::Refused {
                index: EventIndex::ZERO,
                error: TransitionError::UnknownSwap(quote)
            }
        );
        assert!(!verify_replay());
        run_replay_audit();
        assert!(is_halted(), "the audit halts instead of trapping");
    });
}

/// A pocket balance sits in the stable fold that no event produced, and the log spends it.
#[test]
fn replay_audit_halts_on_a_pocket_balance_no_event_produced() {
    on_fresh_memory(|| {
        let quote = QuoteHash::new([8; 32]);
        StableStore(()).put_pocket(
            ChainId::BASE,
            Pocket {
                available: TokenAmount::ZERO,
                reserved: TokenAmount::from(10_u32),
            },
        );
        append(funds(quote));
        append(EventType::PocketSpent {
            quote_hash: quote,
            chain_id: ChainId::BASE,
            amount: TokenAmount::from(10_u32),
        });

        assert_eq!(
            log_replay().unwrap_err(),
            ReplayError::Refused {
                index: EventIndex::new(1),
                error: TransitionError::UnknownPocket(ChainId::BASE)
            }
        );
        assert!(!verify_replay());
        run_replay_audit();
        assert!(is_halted());
    });
}

/// A divergence the log folds cleanly over is still a divergence: the comparison catches
/// what the replay admits.
#[test]
fn replay_audit_halts_on_a_fold_that_replays_cleanly_but_differs() {
    on_fresh_memory(|| {
        let quote = QuoteHash::new([6; 32]);
        append(funds(quote));
        let mut swap = StableStore(()).swap(&quote).unwrap();
        swap.status = SwapStatus::Frozen;
        StableStore(()).put_swap(quote, swap);

        assert!(log_replay().is_ok(), "every event is admissible");
        assert!(!verify_replay());
        run_replay_audit();
        assert!(is_halted());
    });
}
