use super::*;
use crate::state::MemoryStore;
use types::{Attempt, TokenAmount, TxHash};

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
    assert!(stable.matches(&heap));
    assert_eq!(stable.meta().next_event_index, EventIndex::new(4));
    assert_eq!(stable.pocket(&ChainId::BASE).unwrap().reserved, amount(400));
    assert_eq!(
        stable.swap(&quote).unwrap().open_attempt,
        Some(Attempt::FIRST)
    );
}
