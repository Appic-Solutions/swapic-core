use super::*;
use crate::state::MemoryStore;
use types::events::TxPurpose;
use types::{
    BlockNumber, ChainId, EventIndex, GasAmount, Nonce, NonceKey, QuoteHash, Timestamp,
    TokenAmount, TxHash, UnsignedTx, Wei, WeiPerGas,
};

type HeapState = State<MemoryStore>;

const BASE: ChainId = ChainId::BASE;
const ARBITRUM: ChainId = ChainId::ARBITRUM;

fn qh(byte: u8) -> QuoteHash {
    QuoteHash::new([byte; 32])
}

fn amount(value: u128) -> TokenAmount {
    TokenAmount::from(value)
}

/// The key of the `n`th gasless relay the fixtures send: one per relay, as each is signed
/// once.
pub(crate) fn relay_key(n: u64) -> QuoteHash {
    let mut key = [0x7e; 32];
    key[..8].copy_from_slice(&n.to_be_bytes());
    QuoteHash::new(key)
}

/// A transaction the nonce allocator would admit, at `nonce` on `chain`.
pub(crate) fn created(purpose: TxPurpose, chain: ChainId, nonce: u64) -> EventType {
    EventType::TxCreated {
        purpose,
        chain_id: chain,
        nonce: Nonce::new(nonce),
        to: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"
            .parse()
            .unwrap(),
        value: Wei::ZERO,
        data: vec![0xde, 0xad, 0xbe, 0xef],
        gas_limit: GasAmount::from(120_000_u32),
        max_fee: WeiPerGas::from(2_000_000_000_u64),
        max_priority_fee: WeiPerGas::from(100_000_000_u64),
        action: None,
    }
}

/// Relay `n`'s transaction, handed out at `nonce` on Base.
pub(crate) fn relay_created(n: u64, nonce: u64) -> EventType {
    created(TxPurpose::Relay(relay_key(n)), BASE, nonce)
}

/// Relay `n`'s signed record, spending `nonce` on Base: it creates the relay's flow.
pub(crate) fn relayed(n: u64, nonce: u64) -> EventType {
    EventType::Flow {
        quote_hash: relay_key(n),
        step: FlowStep::GaslessRelayed {
            chain_id: BASE,
            nonce: Nonce::new(nonce),
            tx_hash: TxHash::new([n as u8; 32]),
            raw_tx: vec![0x02, 0xf8, 0x6b],
        },
    }
}

/// Relay `n`'s receipt at depth.
pub(crate) fn relay_closed(n: u64, success: bool) -> EventType {
    EventType::Flow {
        quote_hash: relay_key(n),
        step: FlowStep::TxClosed {
            tx_hash: TxHash::new([n as u8; 32]),
            block: BlockNumber::new(9),
            success,
        },
    }
}

fn replaced(purpose: TxPurpose, chain: ChainId, nonce: u64) -> EventType {
    EventType::TxReplaced {
        purpose,
        chain_id: chain,
        nonce: Nonce::new(nonce),
        max_fee: WeiPerGas::from(4_000_000_000_u64),
        max_priority_fee: WeiPerGas::from(200_000_000_u64),
        tx_hash: TxHash::new([7; 32]),
        raw_tx: vec![0x02, 0xf8, 0x6b],
    }
}

fn cancel(nonce: u64) -> EventType {
    EventType::TxCancelled {
        chain_id: BASE,
        nonce: Nonce::new(nonce),
        tx_hash: TxHash::new([9; 32]),
        raw_tx: vec![0x02, 0xf8, 0x6c],
    }
}

/// Seals `payload` as the next event of `state` at `nanos`.
fn next_event(state: &HeapState, nanos: u64, payload: EventType) -> Event {
    let meta = state.meta();
    Event {
        index: meta.next_event_index,
        timestamp: Timestamp::from_nanos(nanos),
        payload: payload,
    }
}

/// Checks and applies `payload` as the next event of `state`, sealed at `nanos`.
fn admit(state: &mut HeapState, nanos: u64, payload: EventType) -> Event {
    state.check(&payload).expect("guard admits");
    let event = next_event(state, nanos, payload);
    apply_state_transition(state, &event);
    event
}

fn fold(events: Vec<EventType>) -> HeapState {
    let mut state = HeapState::default();
    for payload in events {
        admit(&mut state, 1, payload);
    }
    state
}

/// The fold is the same however often the log is replayed.
#[test]
fn replay_is_deterministic() {
    let mut state = HeapState::default();
    let events: Vec<Event> = [relay_created(1, 0), relayed(1, 0), relay_closed(1, true)]
        .into_iter()
        .map(|payload| admit(&mut state, 7, payload))
        .collect();
    assert_eq!(
        replay(events.clone().into_iter()).unwrap(),
        replay(events.into_iter()).unwrap()
    );
}

/// The two stores a fold can land in agree event for event, so the audit can compare them.
#[test]
fn replay_rebuilds_exactly_the_incremental_state() {
    let mut state = HeapState::default();
    let log: Vec<Event> = [
        relay_created(1, 0),
        relayed(1, 0),
        relay_created(2, 1),
        relay_closed(1, true),
    ]
    .into_iter()
    .map(|payload| admit(&mut state, 42, payload))
    .collect();
    assert_eq!(replay(log), Ok(state));
}

/// The replay admits a log the way `append_event` did, so a log a bug or a later wasm
/// broke is an `Err` naming the event, never a panic in the fold.
#[test]
fn replay_refuses_a_log_append_event_could_not_have_written() {
    let mut state = HeapState::default();
    let log: Vec<Event> = [relay_created(1, 0), relayed(1, 0)]
        .into_iter()
        .map(|payload| admit(&mut state, 1, payload))
        .collect();

    // an event the rules refuse: the relay's flow exists, so it is not created twice
    let mut refused = log.clone();
    refused.push(next_event(&state, 1, relay_created(1, 1)));
    assert_eq!(
        replay(refused),
        Err(ReplayError::Refused {
            index: EventIndex::new(2),
            error: types::FlowError::Exists(relay_key(1)).into()
        })
    );

    // a signed record spending a number nothing handed out
    let orphan = next_event(&HeapState::default(), 1, relayed(1, 0));
    assert_eq!(
        replay([orphan]),
        Err(ReplayError::Refused {
            index: EventIndex::ZERO,
            error: TransitionError::NonceNotUnsigned {
                chain_id: BASE,
                nonce: Nonce::ZERO,
            }
        })
    );

    // a gap in the numbering
    let mut gap = log.clone();
    gap.remove(0);
    assert_eq!(
        replay(gap),
        Err(ReplayError::OutOfSequence {
            expected: EventIndex::ZERO,
            found: EventIndex::new(1)
        })
    );
}

/// Rule A4, the one that makes a double spend of a nonce impossible: the allocator counts
/// from zero on each chain, hands out one number at a time, and counts each chain
/// separately.
///
/// One key per allocation, because a key that already holds a number it has not signed for
/// is refused a second one: the two `TxCreated` that drive the allocator here belong to two
/// relays, which is the only way two numbers are ever out at once.
#[test]
fn the_nonce_allocator_hands_out_one_number_at_a_time_per_chain() {
    let relay = |n: u64| TxPurpose::Relay(relay_key(n));
    let state = fold(vec![relay_created(1, 0), relay_created(2, 1)]);
    assert_eq!(state.next_nonce(&BASE), Nonce::new(2));
    assert_eq!(
        state.next_nonce(&ARBITRUM),
        Nonce::ZERO,
        "a chain nothing was sent on starts at zero"
    );

    // the next one on Base must be 2, and nothing else
    for wrong in [0, 1, 3, u64::MAX] {
        assert_eq!(
            state.check(&created(relay(3), BASE, wrong)),
            Err(TransitionError::NonceOutOfSequence {
                chain_id: BASE,
                nonce: Nonce::new(wrong),
                expected: Nonce::new(2),
            })
        );
    }
    assert_eq!(state.check(&created(relay(3), BASE, 2)), Ok(()));
    // and each chain counts on its own
    assert_eq!(state.check(&created(relay(3), ARBITRUM, 0)), Ok(()));
    assert_eq!(
        state.check(&created(relay(3), ARBITRUM, 1)),
        Err(TransitionError::NonceOutOfSequence {
            chain_id: ARBITRUM,
            nonce: Nonce::new(1),
            expected: Nonce::ZERO,
        })
    );
}

/// Rule A4 read from the key's side, which is what makes two sends for ONE key safe: while
/// a key holds a number nothing has signed for, it is refused another, on any chain.
/// Without it two calls that interleave at the signature both allocate, and the second's
/// number is left with no transaction.
#[test]
fn one_key_never_holds_two_nonces_at_once() {
    let key = relay_key(1);
    let relay = TxPurpose::Relay(key);
    let state = fold(vec![relay_created(1, 0)]);
    assert_eq!(
        state.check(&created(relay, BASE, 1)),
        Err(TransitionError::NonceStillUnsigned(key)),
        "the key is still holding nonce 0"
    );
    assert_eq!(
        state.check(&created(relay, ARBITRUM, 0)),
        Err(TransitionError::NonceStillUnsigned(key)),
        "and on any chain, because the key is what holds the number"
    );
}

/// A relay is signed once: once its record created its flow, no transaction is created for
/// it again (rule 4b.4 from this side; Swapic's `used(orderId)` is the other).
#[test]
fn a_relay_is_created_once() {
    let state = fold(vec![relay_created(1, 0), relayed(1, 0)]);
    assert_eq!(
        state.check(&relay_created(1, 1)),
        Err(types::FlowError::Exists(relay_key(1)).into())
    );
    assert_eq!(state.check(&relay_created(2, 1)), Ok(()));
}

/// Rule A5, the half the fold carries: a number leaves the allocator held as unsigned, and
/// exactly two lines end that, the signed record of the transaction it was handed out for
/// and the cancel that spends it instead. A number no line ends is a gap the account can
/// never mine past, so the fold holds it where a pass can find it.
#[test]
fn a_created_nonce_is_unsigned_until_a_signed_record_or_a_cancel_ends_it() {
    let key = NonceKey {
        chain_id: BASE,
        nonce: Nonce::ZERO,
    };
    let mut state = HeapState::default();
    admit(&mut state, 7, relay_created(1, 0));
    assert_eq!(
        state.unsigned_nonces(),
        vec![(
            key,
            UnsignedTx {
                purpose: TxPurpose::Relay(relay_key(1)),
                created_at: Timestamp::from_nanos(7),
                action: None,
            }
        )],
        "the allocation is stamped with the instant the log sealed it, so a replay of the \
         log rebuilds the same wait"
    );

    let mut cancelled = state.clone();
    admit(&mut state, 9, relayed(1, 0));
    assert!(
        state.unsigned_nonces().is_empty(),
        "the signed record spends the number the key was holding"
    );

    // the other way it ends, on a state where the signature never came
    admit(&mut cancelled, 11, cancel(0));
    assert!(cancelled.unsigned_nonces().is_empty());
    assert_eq!(
        cancelled.next_nonce(&BASE),
        Nonce::new(1),
        "a cancel spends the number, it does not give it back"
    );
}

/// A cancel spends a number that was handed out and never signed for, and nothing else:
/// a number nobody allocated is not one to put on a chain, and neither is one whose
/// transaction is already signed and in flight.
#[test]
fn a_cancel_is_refused_on_a_nonce_that_is_not_waiting_for_a_signature() {
    let refused = |nonce: u64| {
        Err(TransitionError::NonceNotUnsigned {
            chain_id: BASE,
            nonce: Nonce::new(nonce),
        })
    };
    assert_eq!(HeapState::default().check(&cancel(0)), refused(0));

    let signed_for = fold(vec![relay_created(1, 0), relayed(1, 0)]);
    assert_eq!(
        signed_for.check(&cancel(0)),
        refused(0),
        "a signed transaction is out there carrying this number"
    );
    assert_eq!(signed_for.check(&cancel(1)), refused(1));
}

/// Folding the log rebuilds the numbers still waiting for a signature exactly, which is
/// what lets the deep audit compare them: an allocation the log holds and the fold does not
/// would be a nonce nothing ever ends.
#[test]
fn a_replay_rebuilds_the_nonces_that_are_still_unsigned() {
    let mut state = HeapState::default();
    let log: Vec<Event> = [
        (3, relay_created(1, 0)),
        (4, relay_created(2, 1)),
        (5, relayed(2, 1)),
    ]
    .into_iter()
    .map(|(at, payload)| admit(&mut state, at, payload))
    .collect();
    assert_eq!(
        state.unsigned_nonces().len(),
        1,
        "relay 1 is still holding its number and relay 2 signed for its own"
    );
    assert_eq!(replay(log), Ok(state));
}

/// An allocator with no successor to move to is a different refusal from a number that is
/// not the one expected: reporting it as the second would say the number is not the number
/// it is.
#[test]
fn an_allocator_out_of_numbers_says_so() {
    let mut store = MemoryStore::default();
    store.put_next_nonce(BASE, Nonce::new(u64::MAX));
    let state = State::new(store);
    assert_eq!(
        state.check(&relay_created(1, u64::MAX)),
        Err(TransitionError::NonceExhausted { chain_id: BASE })
    );
}

/// A replacement keeps the nonce of the transaction it replaces: it must name one already
/// allocated on that chain, and it allocates nothing itself.
#[test]
fn a_replacement_keeps_an_allocated_nonce_and_allocates_none() {
    let relay = TxPurpose::Relay(relay_key(1));
    let state = fold(vec![
        relay_created(1, 0),
        relayed(1, 0),
        replaced(relay, BASE, 0),
    ]);
    assert_eq!(
        state.next_nonce(&BASE),
        Nonce::new(1),
        "a replacement allocates nothing"
    );
    for never_allocated in [1, 2, u64::MAX] {
        assert_eq!(
            state.check(&replaced(relay, BASE, never_allocated)),
            Err(TransitionError::NonceNeverAllocated {
                chain_id: BASE,
                nonce: Nonce::new(never_allocated),
                next: Nonce::new(1),
            })
        );
    }
}

/// The race rule A5's cancel opens: the signature is awaited across consensus rounds on
/// another subnet, and a cancel that spent the number meanwhile has sealed it. The signed
/// record that comes back late finds the number gone, and is refused: without this it
/// would be admitted and its outbox entry would overwrite the cancel's, so two signed
/// transactions would carry one nonce. The same refusal meets a record naming a number
/// handed out for another key.
#[test]
fn a_cancelled_nonce_refuses_the_signature_that_comes_back_late() {
    let gone = |nonce: u64| {
        Err(TransitionError::NonceNotUnsigned {
            chain_id: BASE,
            nonce: Nonce::new(nonce),
        })
    };
    let mut state = HeapState::default();
    let log: Vec<Event> = [(2, relay_created(1, 0)), (3, cancel(0))]
        .into_iter()
        .map(|(at, payload)| admit(&mut state, at, payload))
        .collect();
    assert!(state.unsigned_nonces().is_empty(), "the cancel spent it");
    assert_eq!(
        state.check(&relayed(1, 0)),
        gone(0),
        "the number this signature was made for is gone"
    );
    assert_eq!(replay(log), Ok(state.clone()), "the log folds clean");

    // the key may send again: it allocates the next number, and signs against that one
    admit(&mut state, 4, relay_created(1, 1));
    admit(&mut state, 4, relayed(1, 1));
    assert_eq!(state.next_nonce(&BASE), Nonce::new(2));

    // a number handed out for another key is not this one's to spend
    let other = fold(vec![relay_created(2, 0)]);
    assert_eq!(other.check(&relayed(1, 0)), gone(0));
}

/// A flow in `status` with `landed_tx`, the rest empty: what the fold holds for it.
fn stored_flow(status: types::FlowStatus, landed_tx: Option<TxHash>) -> types::Flow {
    types::Flow {
        kind: types::FlowKind::IcpToEvm,
        status,
        icp_quote: None,
        evm_quote: None,
        held: TokenAmount::ZERO,
        held_ledger: None,
        held_in_subaccount: false,
        begun_at: None,
        withdrawal_id: None,
        withdrawn: None,
        eth_held: TokenAmount::ZERO,
        landed_tx,
        chain_id: None,
        tx_hash: None,
        tx_open: false,
        tx_block: None,
        continuation: None,
        failed_continuations: 0,
        delivered: None,
        reason: None,
        created_at: Timestamp::from_nanos(1),
        signed_id: None,
        source_tx: None,
        arrival: None,
        gas_action: None,
    }
}

/// A transfer is credited to one flow only: a `Landed` line naming a transfer another flow
/// already landed is refused with that flow named, one naming a fresh transfer is not.
#[test]
fn a_transfer_landed_for_one_flow_is_refused_for_another() {
    let landed = TxHash::new([0x1a; 32]);
    let mut store = MemoryStore::default();
    store.put_flow(qh(1), stored_flow(types::FlowStatus::Landed, Some(landed)));
    store.put_flow(qh(2), stored_flow(types::FlowStatus::Withdrawn, None));
    let state = HeapState::new(store);
    let land = |tx_hash| EventType::Flow {
        quote_hash: qh(2),
        step: FlowStep::Landed {
            chain_id: BASE,
            tx_hash,
            amount: amount(5),
        },
    };
    assert_eq!(
        state.check(&land(landed)),
        Err(types::FlowError::LandedTwice(landed, qh(1)).into())
    );
    assert_eq!(state.check(&land(TxHash::new([0x1b; 32]))), Ok(()));
}
