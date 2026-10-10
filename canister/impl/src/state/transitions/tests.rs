use super::*;
use types::events::{Choice, TxPurpose};
use types::{
    Attempt, BlockNumber, ChainId, EventHash, EventIndex, GasAmount, GasMode, LedgerMeta, Nonce,
    NonceKey, Pocket, PocketError, QuoteHash, Rail, Swap, SwapStatus, Timestamp, TokenAmount,
    TxHash, UnixSeconds, UnsignedTx, WaitingKey, Wei, WeiPerGas,
};

type HeapState = State<MemoryStore>;

const BASE: ChainId = ChainId::BASE;
const ARBITRUM: ChainId = ChainId::ARBITRUM;

/// An id no quote hashes to: what an event about a swap the log never funded carries.
fn qh(byte: u8) -> QuoteHash {
    QuoteHash::new([byte; 32])
}

fn amount(value: u128) -> TokenAmount {
    TokenAmount::from(value)
}

/// A quote the `FundsReceived` guard accepts, one per nonce. Shared with the other test
/// modules of this crate: every swap starts with a `FundsReceived` whose bytes hash to its
/// id, so every fixture needs a real quote behind its swap.
pub(crate) fn quote(nonce: u64) -> Quote {
    Quote {
        version: 1,
        src_chain: BASE,
        src_token: "usdc".parse().unwrap(),
        amount_in: amount(100),
        dst_chain: ARBITRUM,
        dst_token: "usdc".parse().unwrap(),
        expected_out: amount(99),
        min_out: amount(98),
        dst_address: "0xuser".parse().unwrap(),
        refund_address: None,
        auto_refund: true,
        gas_mode: GasMode::Gasless,
        rail: Rail::CctpV2Fast,
        expires_at: UnixSeconds::new(1_800_000_000),
        nonce,
    }
}

/// The quote at `nonce`, asking to be consulted rather than refunded on its own.
pub(crate) fn manual_quote(nonce: u64) -> Quote {
    Quote {
        auto_refund: false,
        ..quote(nonce)
    }
}

/// The swap id of the quote at `nonce`.
pub(crate) fn swap_id(nonce: u64) -> QuoteHash {
    id_of(&quote(nonce))
}

/// The swap id of the consult-me quote at `nonce`.
pub(crate) fn manual_swap_id(nonce: u64) -> QuoteHash {
    id_of(&manual_quote(nonce))
}

fn id_of(q: &Quote) -> QuoteHash {
    q.hash().expect("a fixture quote has an id")
}

/// The event that funds the swap of the quote at `nonce`.
pub(crate) fn funds(nonce: u64) -> EventType {
    funds_of(&quote(nonce), nonce)
}

/// The event that funds the swap of the consult-me quote at `nonce`.
pub(crate) fn funds_manual(nonce: u64) -> EventType {
    funds_of(&manual_quote(nonce), nonce)
}

fn funds_of(q: &Quote, nonce: u64) -> EventType {
    EventType::FundsReceived {
        quote_hash: id_of(q),
        quote_bytes: q.canonical_bytes().expect("a fixture quote has a preimage"),
        chain_id: q.src_chain,
        token: q.src_token.clone(),
        amount: q.amount_in,
        tx_ref: format!("0xabc{nonce}"),
    }
}

fn signed(qh: QuoteHash, attempt: u32) -> EventType {
    EventType::TxSigned {
        quote_hash: qh,
        attempt: Attempt::new(attempt),
        chain_id: BASE,
        tx_hash: TxHash::new([attempt as u8; 32]),
        raw_tx: vec![],
    }
}

fn confirmed(qh: QuoteHash, attempt: u32) -> EventType {
    EventType::TxConfirmed {
        quote_hash: qh,
        attempt: Attempt::new(attempt),
        chain_id: BASE,
        tx_hash: TxHash::new([attempt as u8; 32]),
        block: BlockNumber::new(1),
    }
}

/// Seals `payload` as the next event of `state` at `nanos`.
fn next_event(state: &HeapState, nanos: u64, payload: EventType) -> Event {
    let meta = state.meta();
    Event::seal(
        meta.next_event_index,
        Timestamp::from_nanos(nanos),
        meta.last_event_hash,
        payload,
    )
    .expect("the payload has a preimage")
}

fn fold(events: Vec<EventType>) -> HeapState {
    let mut state = HeapState::default();
    for e in events {
        state.check(&e).expect("guard admits");
        let event = next_event(&state, 1, e);
        apply_state_transition(&mut state, &event);
    }
    state
}

fn swap(state: &HeapState, qh: QuoteHash) -> Swap {
    state.swap(&qh).expect("the swap exists")
}

fn pocket(state: &HeapState, chain_id: ChainId) -> Pocket {
    state.pocket(&chain_id).expect("the pocket exists")
}

#[test]
fn happy_path_reaches_done() {
    let qh = swap_id(1);
    let state = fold(vec![
        funds(1),
        allocated(qh, 0),
        signed(qh, 1),
        confirmed(qh, 1),
        EventType::PaidInStable {
            quote_hash: qh,
            chain_id: ARBITRUM,
            amount: amount(99),
        },
        allocated(qh, 1),
        signed(qh, 2),
        confirmed(qh, 2),
        EventType::SwapDone { quote_hash: qh },
    ]);
    let swap = swap(&state, qh);
    assert_eq!(swap.status, SwapStatus::Done);
    assert_eq!(swap.last_attempt, Some(Attempt::new(2)));
    assert_eq!(swap.amount_paid, Some(amount(99)));
}

#[test]
fn cannot_sign_next_attempt_while_one_is_open() {
    let qh = swap_id(1);
    let mut state = fold(vec![funds(1), allocated(qh, 0), signed(qh, 1)]);
    assert_eq!(
        state.check(&signed(qh, 2)),
        Err(TransitionError::AttemptStillOpen(Attempt::FIRST))
    );
    // closing attempt 1 unblocks attempt 2, once it holds a number to sign against
    for payload in [confirmed(qh, 1), allocated(qh, 1)] {
        let env = next_event(&state, 1, payload);
        apply_state_transition(&mut state, &env);
    }
    assert!(state.check(&signed(qh, 2)).is_ok());
}

#[test]
fn attempt_numbers_are_strictly_sequential() {
    let state = fold(vec![funds(1)]);
    // skips 1
    assert_eq!(
        state.check(&signed(swap_id(1), 2)),
        Err(TransitionError::AttemptOutOfSequence {
            attempt: Attempt::new(2),
            expected: Some(Attempt::FIRST)
        })
    );
}

#[test]
fn closed_swap_rejects_new_signatures() {
    let qh = swap_id(1);
    let state = fold(vec![
        funds(1),
        EventType::Frozen {
            quote_hash: qh,
            reason: "sanctions".into(),
        },
    ]);
    assert_eq!(
        state.check(&signed(qh, 1)),
        Err(TransitionError::SwapClosed(SwapStatus::Frozen))
    );
}

#[test]
fn duplicate_funds_received_rejected() {
    let state = fold(vec![funds(1)]);
    assert_eq!(
        state.check(&funds(1)),
        Err(TransitionError::SwapExists(swap_id(1)))
    );
}

/// The swap id and the preimage recorded with it must bind, because the expiry sweep reads
/// `auto_refund` out of those bytes: a mismatched pair would refund a user who asked to be
/// consulted, or leave a swap waiting for an answer nobody will give.
#[test]
fn funds_received_refuses_a_swap_id_its_quote_bytes_do_not_hash_to() {
    let state = HeapState::default();
    let EventType::FundsReceived {
        quote_bytes,
        chain_id,
        token,
        amount,
        tx_ref,
        ..
    } = funds(1)
    else {
        panic!("the fixture is a FundsReceived");
    };
    // the bytes of quote 1 under the id of quote 2
    let swapped = EventType::FundsReceived {
        quote_hash: swap_id(2),
        quote_bytes,
        chain_id,
        token,
        amount,
        tx_ref,
    };
    assert_eq!(
        state.check(&swapped),
        Err(TransitionError::QuoteHashMismatch {
            declared: swap_id(2),
            computed: swap_id(1)
        })
    );
    state.check(&funds(1)).expect("the matching pair passes");
}

/// Bytes the canister cannot read are no quote at all: the refund policy would have nothing
/// to read out of them, and the id could not be checked against them either.
#[test]
fn funds_received_refuses_quote_bytes_that_are_not_a_quote() {
    let state = HeapState::default();
    let garbled = EventType::FundsReceived {
        quote_hash: swap_id(1),
        quote_bytes: vec![0xff; 9],
        chain_id: BASE,
        token: "usdc".parse().unwrap(),
        amount: amount(100),
        tx_ref: "0xabc".into(),
    };
    assert!(matches!(
        state.check(&garbled),
        Err(TransitionError::UnparseableQuote(_))
    ));
}

/// Rule A2 for the line that creates money: what the line says arrived must be what the
/// quote said would, on the quote's own chain, in its own token and its own amount, so no
/// writer can record a swap the quote does not describe. The token is compared as a token
/// and never as text: the vault logs an address in its checksummed spelling, and a quote
/// may spell the same address in lower case.
#[test]
fn funds_received_must_be_the_quotes_chain_token_and_amount() {
    let state = HeapState::default();
    let EventType::FundsReceived {
        quote_hash,
        quote_bytes,
        tx_ref,
        ..
    } = funds(1)
    else {
        panic!("the fixture is a FundsReceived");
    };
    let line = |chain_id: ChainId, token: &str, amount_in: u128| EventType::FundsReceived {
        quote_hash,
        quote_bytes: quote_bytes.clone(),
        chain_id,
        token: token.parse().unwrap(),
        amount: amount(amount_in),
        tx_ref: tx_ref.clone(),
    };
    assert_eq!(
        state.check(&line(ARBITRUM, "usdc", 100)),
        Err(TransitionError::FundsChainNotTheQuotes {
            logged: ARBITRUM,
            quoted: BASE,
        })
    );
    assert_eq!(
        state.check(&line(BASE, "usdt", 100)),
        Err(TransitionError::FundsTokenNotTheQuotes {
            logged: "usdt".parse().unwrap(),
            quoted: "usdc".parse().unwrap(),
        })
    );
    assert_eq!(
        state.check(&line(BASE, "usdc", 99)),
        Err(TransitionError::FundsAmountNotTheQuotes {
            logged: amount(99),
            quoted: amount(100),
        })
    );
    assert_eq!(state.check(&line(BASE, "usdc", 100)), Ok(()));

    // the vault's checksummed spelling of the quote's lower-case token is the same token
    let checksummed = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913";
    let evm = Quote {
        src_token: checksummed.to_ascii_lowercase().parse().unwrap(),
        ..quote(1)
    };
    let logged = EventType::FundsReceived {
        quote_hash: id_of(&evm),
        quote_bytes: evm.canonical_bytes().unwrap(),
        chain_id: BASE,
        token: checksummed.parse().unwrap(),
        amount: amount(100),
        tx_ref: "0xabc".into(),
    };
    assert_eq!(state.check(&logged), Ok(()));
}

#[test]
fn replay_is_deterministic() {
    let qh = swap_id(1);
    let events: Vec<Event> = {
        let mut state = HeapState::default();
        let mut out = vec![];
        for e in [funds(1), allocated(qh, 0), signed(qh, 1), confirmed(qh, 1)] {
            let env = next_event(&state, 7, e);
            apply_state_transition(&mut state, &env);
            out.push(env);
        }
        out
    };
    assert_eq!(
        replay(events.clone().into_iter()).unwrap(),
        replay(events.into_iter()).unwrap()
    );
}

#[test]
fn pocket_reserve_moves_available_to_reserved() {
    let state = fold(vec![
        EventType::PocketFunded {
            chain_id: BASE,
            amount: amount(1000),
        },
        funds(1),
        EventType::PocketReserved {
            quote_hash: swap_id(1),
            chain_id: BASE,
            amount: amount(400),
        },
    ]);
    assert_eq!(pocket(&state, BASE).available, amount(600));
    assert_eq!(pocket(&state, BASE).reserved, amount(400));
}

fn failed(qh: QuoteHash, attempt: u32) -> EventType {
    EventType::TxFailed {
        quote_hash: qh,
        attempt: Attempt::new(attempt),
        reason: "reverted".into(),
    }
}

fn decide(qh: QuoteHash, choice: Choice) -> Vec<EventType> {
    vec![
        ask(qh),
        EventType::DecisionMade {
            quote_hash: qh,
            choice,
        },
    ]
}

fn paid(qh: QuoteHash, value: u128) -> EventType {
    EventType::PaidInStable {
        quote_hash: qh,
        chain_id: ARBITRUM,
        amount: amount(value),
    }
}

// A requote returns the swap to Executing, which re-opens the PaidInStable arm;
// without the amount_paid check the second event would overwrite the first amount.
#[test]
fn second_paid_in_stable_rejected() {
    let qh = swap_id(1);
    let mut events = vec![
        funds(1),
        allocated(qh, 0),
        signed(qh, 1),
        confirmed(qh, 1),
        paid(qh, 99),
    ];
    events.extend(decide(qh, Choice::Requote));
    let state = fold(events);
    assert_eq!(swap(&state, qh).status, SwapStatus::Executing);
    assert_eq!(
        state.check(&paid(qh, 50)),
        Err(TransitionError::AlreadyPaid)
    );
    assert_eq!(swap(&state, qh).amount_paid, Some(amount(99)));
}

/// Paid is a fact, not an amount: a payment of zero followed by a requote must not open the
/// door to a second payment that overwrites the first.
#[test]
fn a_zero_payment_still_refuses_a_second_after_a_requote() {
    let qh = swap_id(1);
    let mut events = vec![
        funds(1),
        allocated(qh, 0),
        signed(qh, 1),
        confirmed(qh, 1),
        paid(qh, 0),
    ];
    events.extend(decide(qh, Choice::Requote));
    let state = fold(events);
    assert_eq!(swap(&state, qh).status, SwapStatus::Executing);
    assert_eq!(swap(&state, qh).amount_paid, Some(TokenAmount::ZERO));
    assert_eq!(
        state.check(&paid(qh, 50)),
        Err(TransitionError::AlreadyPaid)
    );
}

/// No event can carry an amount a 16-byte canonical field cannot hold, so the guard
/// refuses it with a typed error before anything could try to seal it.
#[test]
fn check_refuses_an_amount_no_canonical_field_holds() {
    let state = HeapState::default();
    let fund = EventType::PocketFunded {
        chain_id: BASE,
        amount: TokenAmount::MAX,
    };
    assert_eq!(
        state.check(&fund),
        Err(TransitionError::AmountOutOfRange(TokenAmount::MAX))
    );
    let at_max = EventType::PocketFunded {
        chain_id: BASE,
        amount: amount(u128::MAX),
    };
    assert_eq!(state.check(&at_max), Ok(()));
}

#[test]
fn waiting_for_user_blocks_signing() {
    let qh = swap_id(1);
    let mut state = fold(vec![
        funds(1),
        allocated(qh, 0),
        signed(qh, 1),
        confirmed(qh, 1),
        ask(qh),
    ]);
    assert_eq!(
        state.check(&signed(qh, 2)),
        Err(TransitionError::WaitingForUser)
    );
    // answering the question unblocks the next attempt, which allocates and then signs
    let resume = EventType::DecisionMade {
        quote_hash: qh,
        choice: Choice::Requote,
    };
    for payload in [resume, allocated(qh, 1)] {
        let env = next_event(&state, 1, payload);
        apply_state_transition(&mut state, &env);
    }
    assert!(state.check(&signed(qh, 2)).is_ok());
}

#[test]
fn done_swap_rejects_freeze() {
    let qh = swap_id(1);
    let state = fold(vec![
        funds(1),
        allocated(qh, 0),
        signed(qh, 1),
        confirmed(qh, 1),
        EventType::SwapDone { quote_hash: qh },
    ]);
    assert_eq!(swap(&state, qh).status, SwapStatus::Done);
    let freeze = EventType::Frozen {
        quote_hash: qh,
        reason: "sanctions".into(),
    };
    assert_eq!(
        state.check(&freeze),
        Err(TransitionError::SwapClosed(SwapStatus::Done))
    );
}

#[test]
fn fee_needs_a_swap_but_config_and_funding_do_not() {
    let state = HeapState::default();
    let fee = EventType::FeeAccrued {
        quote_hash: qh(9),
        amount: amount(7),
    };
    assert_eq!(state.check(&fee), Err(TransitionError::UnknownSwap(qh(9))));
    assert!(state
        .check(&EventType::ConfigChanged { json: "{}".into() })
        .is_ok());
    assert!(state
        .check(&EventType::PocketFunded {
            chain_id: BASE,
            amount: amount(1),
        })
        .is_ok());
}

#[test]
fn tx_failed_closes_the_attempt() {
    let qh = swap_id(1);
    let mut state = fold(vec![
        funds(1),
        allocated(qh, 0),
        signed(qh, 1),
        failed(qh, 1),
    ]);
    assert_eq!(swap(&state, qh).open_attempt, None);
    assert_eq!(swap(&state, qh).last_attempt, Some(Attempt::FIRST));
    // a failed attempt still counts, so the retry is number 2, at the next number
    let env = next_event(&state, 1, allocated(qh, 1));
    apply_state_transition(&mut state, &env);
    assert!(matches!(
        state.check(&signed(qh, 1)),
        Err(TransitionError::AttemptOutOfSequence { .. })
    ));
    assert!(state.check(&signed(qh, 2)).is_ok());
}

#[test]
fn refund_path_reaches_refunded() {
    let qh = swap_id(1);
    let state = fold(vec![
        funds(1),
        EventType::RefundStarted {
            quote_hash: qh,
            reason: "timeout".into(),
        },
        allocated(qh, 0),
        signed(qh, 1),
        confirmed(qh, 1),
        EventType::Refunded {
            quote_hash: qh,
            chain_id: BASE,
            token: "usdc".parse().unwrap(),
            amount: amount(100),
            to: "0xuser".parse().unwrap(),
        },
    ]);
    assert_eq!(swap(&state, qh).status, SwapStatus::Refunded);
    // Refunded is terminal
    assert_eq!(
        state.check(&signed(qh, 2)),
        Err(TransitionError::SwapClosed(SwapStatus::Refunded))
    );
}

#[test]
fn pocket_release_returns_reserved_to_available() {
    let qh = swap_id(1);
    let state = fold(vec![
        EventType::PocketFunded {
            chain_id: BASE,
            amount: amount(1000),
        },
        funds(1),
        EventType::PocketReserved {
            quote_hash: qh,
            chain_id: BASE,
            amount: amount(400),
        },
        EventType::PocketReleased {
            quote_hash: qh,
            chain_id: BASE,
            amount: amount(150),
        },
    ]);
    assert_eq!(pocket(&state, BASE).available, amount(750));
    assert_eq!(pocket(&state, BASE).reserved, amount(250));
    // cannot release more than is reserved
    let release = EventType::PocketReleased {
        quote_hash: qh,
        chain_id: BASE,
        amount: amount(300),
    };
    assert_eq!(
        state.check(&release),
        Err(TransitionError::Pocket(PocketError::InsufficientReserved {
            reserved: amount(250),
            requested: amount(300)
        }))
    );
}

fn ask(qh: QuoteHash) -> EventType {
    EventType::DecisionRequired {
        quote_hash: qh,
        reason: "slippage".into(),
    }
}

// A swap that stops waiting must stop the clock too, or Task 7's expiry sweep would
// keep seeing a stale deadline on a swap that is no longer waiting for anyone.
#[test]
fn refund_stops_the_waiting_clock() {
    let qh = swap_id(1);
    let state = fold(vec![
        funds(1),
        ask(qh),
        EventType::RefundStarted {
            quote_hash: qh,
            reason: "timeout".into(),
        },
    ]);
    assert_eq!(swap(&state, qh).status, SwapStatus::Refunding);
    assert_eq!(swap(&state, qh).waiting_since, None);
}

#[test]
fn freeze_stops_the_waiting_clock() {
    let qh = swap_id(1);
    let state = fold(vec![
        funds(1),
        ask(qh),
        EventType::Frozen {
            quote_hash: qh,
            reason: "sanctions".into(),
        },
    ]);
    assert_eq!(swap(&state, qh).status, SwapStatus::Frozen);
    assert_eq!(swap(&state, qh).waiting_since, None);
}

#[test]
fn second_decision_request_rejected() {
    let qh = swap_id(1);
    let state = fold(vec![funds(1), ask(qh)]);
    // re-asking would silently re-arm the deadline
    assert_eq!(state.check(&ask(qh)), Err(TransitionError::WaitingForUser));
}

/// The sequence that used to turn a refund into a delivery: the refund is in flight, the
/// swap is asked a question, the user requotes, and the refund that lands is recorded as a
/// delivery. It is refused at the question, which is the only step that could start it.
#[test]
fn a_refund_in_flight_cannot_be_turned_back_into_a_delivery() {
    let qh = swap_id(1);
    let state = fold(vec![
        funds(1),
        EventType::RefundStarted {
            quote_hash: qh,
            reason: "decision timeout".into(),
        },
        allocated(qh, 0),
        signed(qh, 1),
    ]);
    assert_eq!(swap(&state, qh).status, SwapStatus::Refunding);
    assert_eq!(
        state.check(&ask(qh)),
        Err(TransitionError::CannotAskWhileRefunding),
        "the refund is on its way back to the user"
    );

    // and once the refund attempt has landed, the swap is still refunding and still unaskable
    let mut state = state;
    let event = next_event(&state, 1, confirmed(qh, 1));
    apply_state_transition(&mut state, &event);
    assert_eq!(
        state.check(&ask(qh)),
        Err(TransitionError::CannotAskWhileRefunding)
    );
}

/// A question put while an attempt is in flight would be answered while the chain is still
/// deciding that attempt, so the answer would race the transaction.
#[test]
fn a_swap_with_an_attempt_in_flight_is_not_asked() {
    let qh = swap_id(1);
    let mut state = fold(vec![funds(1), allocated(qh, 0), signed(qh, 1)]);
    assert_eq!(
        state.check(&ask(qh)),
        Err(TransitionError::AttemptStillOpen(Attempt::FIRST))
    );
    // closing the attempt opens the question
    let event = next_event(&state, 1, failed(qh, 1));
    apply_state_transition(&mut state, &event);
    assert!(state.check(&ask(qh)).is_ok());
}

/// A refund that failed is retried as the next refund attempt, so refusing the question
/// takes nothing away from the refund path.
#[test]
fn a_failed_refund_attempt_is_retried_as_the_next_attempt() {
    let qh = swap_id(1);
    let state = fold(vec![
        funds(1),
        EventType::RefundStarted {
            quote_hash: qh,
            reason: "decision timeout".into(),
        },
        allocated(qh, 0),
        signed(qh, 1),
        failed(qh, 1),
        allocated(qh, 1),
        signed(qh, 2),
        confirmed(qh, 2),
        EventType::Refunded {
            quote_hash: qh,
            chain_id: BASE,
            token: "usdc".parse().unwrap(),
            amount: amount(100),
            to: "0xuser".parse().unwrap(),
        },
    ]);
    assert_eq!(swap(&state, qh).status, SwapStatus::Refunded);
    assert_eq!(swap(&state, qh).last_attempt, Some(Attempt::new(2)));
}

#[test]
fn decision_to_refund_reaches_refunding() {
    let qh = swap_id(1);
    let mut events = vec![funds(1)];
    events.extend(decide(qh, Choice::Refund));
    let state = fold(events);
    assert_eq!(swap(&state, qh).status, SwapStatus::Refunding);
    assert_eq!(swap(&state, qh).waiting_since, None);
}

// Unlike a release, a spend does not return the value: the pocket total drops.
#[test]
fn pocket_spend_debits_reserved_without_returning_it() {
    let qh = swap_id(1);
    let state = fold(vec![
        EventType::PocketFunded {
            chain_id: BASE,
            amount: amount(1000),
        },
        funds(1),
        EventType::PocketReserved {
            quote_hash: qh,
            chain_id: BASE,
            amount: amount(400),
        },
        EventType::PocketSpent {
            quote_hash: qh,
            chain_id: BASE,
            amount: amount(250),
        },
    ]);
    let p = pocket(&state, BASE);
    assert_eq!(p.available, amount(600));
    assert_eq!(p.reserved, amount(150));
    assert_eq!(
        p.available.checked_add(p.reserved),
        Some(amount(750)),
        "250 left the pocket system"
    );
    // cannot spend more than is reserved
    let spend = EventType::PocketSpent {
        quote_hash: qh,
        chain_id: BASE,
        amount: amount(200),
    };
    assert_eq!(
        state.check(&spend),
        Err(TransitionError::Pocket(PocketError::InsufficientReserved {
            reserved: amount(150),
            requested: amount(200)
        }))
    );
}

#[test]
fn pocket_reserve_for_unknown_swap_rejected() {
    let state = fold(vec![EventType::PocketFunded {
        chain_id: BASE,
        amount: amount(1000),
    }]);
    let reserve = EventType::PocketReserved {
        quote_hash: qh(9),
        chain_id: BASE,
        amount: amount(400),
    };
    assert_eq!(
        state.check(&reserve),
        Err(TransitionError::UnknownSwap(qh(9)))
    );
}

#[test]
fn pocket_rebalance_moves_available_between_chains() {
    let state = fold(vec![
        EventType::PocketFunded {
            chain_id: BASE,
            amount: amount(1000),
        },
        EventType::PocketRebalanced {
            from_chain: BASE,
            to_chain: ARBITRUM,
            amount: amount(250),
            route: "cctp".into(),
        },
    ]);
    assert_eq!(pocket(&state, BASE).available, amount(750));
    assert_eq!(pocket(&state, ARBITRUM).available, amount(250));
    // the source chain cannot send what it does not have
    let rebalance = EventType::PocketRebalanced {
        from_chain: BASE,
        to_chain: ARBITRUM,
        amount: amount(751),
        route: "cctp".into(),
    };
    assert_eq!(
        state.check(&rebalance),
        Err(TransitionError::Pocket(
            PocketError::InsufficientAvailable {
                available: amount(750),
                requested: amount(751)
            }
        ))
    );
}

// The waiting clock is the only place apply reads the event timestamp, so seal this one
// by hand with a distinctive time instead of fold's constant 1.
#[test]
fn decision_sets_then_clears_the_waiting_clock() {
    let qh = swap_id(1);
    let mut state = fold(vec![funds(1)]);
    let pause = ask(qh);
    state.check(&pause).expect("guard admits pause");
    let env = next_event(&state, 777, pause);
    apply_state_transition(&mut state, &env);
    assert_eq!(swap(&state, qh).status, SwapStatus::WaitingForUser);
    assert_eq!(
        swap(&state, qh).waiting_since,
        Some(Timestamp::from_nanos(777))
    );

    let resume = EventType::DecisionMade {
        quote_hash: qh,
        choice: Choice::Requote,
    };
    state.check(&resume).expect("guard admits resume");
    let env = next_event(&state, 900, resume);
    apply_state_transition(&mut state, &env);
    assert_eq!(swap(&state, qh).status, SwapStatus::Executing);
    assert_eq!(swap(&state, qh).waiting_since, None);
}

/// A rebalance onto its own chain withdraws, then funds the same pocket back: the guard
/// runs the two steps in the order the fold does, so neither panics nor double-counts.
#[test]
fn a_rebalance_onto_the_same_chain_nets_to_nothing() {
    let state = fold(vec![
        EventType::PocketFunded {
            chain_id: BASE,
            amount: amount(1000),
        },
        EventType::PocketRebalanced {
            from_chain: BASE,
            to_chain: BASE,
            amount: amount(1000),
            route: "loop".into(),
        },
    ]);
    assert_eq!(pocket(&state, BASE).available, amount(1000));
}

/// Every overflow the fold could hit is refused by the guard first, so applying never
/// panics on an event the guard admitted. No log of u128 amounts gets near u256::MAX, so
/// the balances are placed there directly.
#[test]
fn the_guard_refuses_what_would_overflow_the_fold() {
    let qh = swap_id(1);
    let mut store = MemoryStore::default();
    store.put_swap(qh, swap(&fold(vec![funds(1)]), qh));
    store.put_meta(LedgerMeta {
        fees_accrued: TokenAmount::MAX,
        ..LedgerMeta::default()
    });
    store.put_pocket(
        BASE,
        Pocket {
            available: TokenAmount::MAX,
            reserved: TokenAmount::ZERO,
        },
    );
    let state = State::new(store);
    let fee = EventType::FeeAccrued {
        quote_hash: qh,
        amount: amount(1),
    };
    assert_eq!(state.check(&fee), Err(TransitionError::FeesOverflow));
    let fund = EventType::PocketFunded {
        chain_id: BASE,
        amount: amount(1),
    };
    assert_eq!(
        state.check(&fund),
        Err(TransitionError::Pocket(PocketError::Overflow))
    );
}

/// The two stores a fold can land in agree event for event, so the audit can compare them.
#[test]
fn replay_rebuilds_exactly_the_incremental_state() {
    let qh = swap_id(1);
    let mut state = HeapState::default();
    let mut log = vec![];
    for payload in [
        EventType::PocketFunded {
            chain_id: BASE,
            amount: amount(1000),
        },
        funds(1),
        allocated(qh, 0),
        signed(qh, 1),
        // the attempt closes before the question: a swap with one in flight is not asked
        confirmed(qh, 1),
        ask(qh),
    ] {
        state.check(&payload).expect("guard admits");
        let event = next_event(&state, 42, payload);
        apply_state_transition(&mut state, &event);
        log.push(event);
    }
    assert_eq!(replay(log), Ok(state));
}

/// The replay admits a log the way `append_event` did, so a log a bug or a later wasm
/// broke is an `Err` naming the event, never a panic in the fold.
#[test]
fn replay_refuses_a_log_append_event_could_not_have_written() {
    let qh = swap_id(1);
    let mut state = HeapState::default();
    let mut log = vec![];
    for payload in [funds(1), allocated(qh, 0), signed(qh, 1)] {
        let event = next_event(&state, 1, payload);
        apply_state_transition(&mut state, &event);
        log.push(event);
    }

    // an event the rules refuse: attempt 1 is still open
    let mut refused = log.clone();
    refused.push(next_event(&state, 1, signed(qh, 2)));
    assert_eq!(
        replay(refused),
        Err(ReplayError::Refused {
            index: EventIndex::new(3),
            error: TransitionError::AttemptStillOpen(Attempt::FIRST)
        })
    );

    // an event on a swap the log never funded
    let orphan = next_event(&HeapState::default(), 1, signed(qh, 1));
    assert_eq!(
        replay([orphan]),
        Err(ReplayError::Refused {
            index: EventIndex::ZERO,
            error: TransitionError::UnknownSwap(qh)
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

    // a link to a parent the fold does not end with
    let mut unlinked = log.clone();
    unlinked[1].parent_hash = EventHash::new([7; 32]);
    assert!(matches!(
        replay(unlinked),
        Err(ReplayError::Unlinked { .. })
    ));
}

/// The repair makes a swap's index entries agree with the swap: every entry the swap does
/// not imply goes, the one it does stays or comes back, and a swap whose entries already
/// agree is left as it is. So it is admitted on any swap, or on none, and it can never
/// strand a wait.
#[test]
fn a_repair_makes_the_index_agree_with_the_swap() {
    let [waiting, settled, manual] = [swap_id(1), swap_id(2), manual_swap_id(3)];
    let orphan = qh(9);
    let key = |nanos: u64, quote_hash| WaitingKey {
        since: Timestamp::from_nanos(nanos),
        quote_hash,
    };
    let mut state = fold(vec![funds(1), funds(2), funds_manual(3)]);
    for (nanos, quote_hash) in [(100, waiting), (101, manual)] {
        let pause = ask(quote_hash);
        state.check(&pause).expect("guard admits pause");
        let event = next_event(&state, nanos, pause);
        apply_state_transition(&mut state, &event);
    }
    assert_eq!(
        state.store().auto_refund_waiting(),
        vec![key(100, waiting)],
        "the consult-me wait is in no index"
    );

    // entries no event produced: the real wait under an instant it never had, a swap that
    // never waited, the consult-me swap, and no swap at all
    let mut store = state.into_store();
    for planted in [
        key(1, waiting),
        key(2, settled),
        key(3, manual),
        key(4, orphan),
    ] {
        store.put_auto_refund_waiting(planted);
    }
    let mut state = State::new(store);
    let repair = |state: &mut HeapState, quote_hash| {
        let repair = EventType::WaitingRepaired { quote_hash };
        assert_eq!(
            state.check(&repair),
            Ok(()),
            "admitted on any swap, or none"
        );
        let event = next_event(state, 200, repair);
        apply_state_transition(state, &event);
    };
    for quote_hash in [waiting, settled, manual, orphan] {
        repair(&mut state, quote_hash);
    }
    assert_eq!(
        state.store().auto_refund_waiting(),
        vec![key(100, waiting)],
        "the one wait the swap implies, and nothing else"
    );

    // where the index already agrees the repair changes nothing, and a lost entry comes back
    repair(&mut state, waiting);
    assert_eq!(state.store().auto_refund_waiting(), vec![key(100, waiting)]);
    let mut store = state.into_store();
    store.remove_auto_refund_waiting(&key(100, waiting));
    let mut state = State::new(store);
    repair(&mut state, waiting);
    assert_eq!(state.store().auto_refund_waiting(), vec![key(100, waiting)]);
}

/// A transaction the nonce allocator would admit, at `nonce` on `chain`.
fn created(purpose: TxPurpose, chain: ChainId, nonce: u64) -> EventType {
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

/// Rule A4, the one that makes a double spend of a nonce impossible: the allocator counts
/// from zero on each chain, hands out one number at a time, and counts each chain
/// separately.
///
/// One swap per allocation, because a swap that already holds a number it has not signed
/// for is refused a second one: the two `TxCreated` that drive the allocator here belong to
/// two swaps, which is the only way two numbers are ever out at once.
#[test]
fn the_nonce_allocator_hands_out_one_number_at_a_time_per_chain() {
    let burn = |n: u64| TxPurpose::Burn(swap_id(n));
    let state = fold(vec![
        funds(1),
        funds(2),
        funds(3),
        created(burn(1), BASE, 0),
        created(burn(2), BASE, 1),
    ]);
    assert_eq!(state.next_nonce(&BASE), Nonce::new(2));
    assert_eq!(
        state.next_nonce(&ARBITRUM),
        Nonce::ZERO,
        "a chain nothing was sent on starts at zero"
    );

    // the next one on Base must be 2, and nothing else
    for wrong in [0, 1, 3, u64::MAX] {
        assert_eq!(
            state.check(&created(burn(3), BASE, wrong)),
            Err(TransitionError::NonceOutOfSequence {
                chain_id: BASE,
                nonce: Nonce::new(wrong),
                expected: Nonce::new(2),
            })
        );
    }
    assert_eq!(state.check(&created(burn(3), BASE, 2)), Ok(()));
    // and each chain counts on its own
    assert_eq!(state.check(&created(burn(3), ARBITRUM, 0)), Ok(()));
    assert_eq!(
        state.check(&created(burn(3), ARBITRUM, 1)),
        Err(TransitionError::NonceOutOfSequence {
            chain_id: ARBITRUM,
            nonce: Nonce::new(1),
            expected: Nonce::ZERO,
        })
    );
}

/// Rule A4 read from the swap's side, which is what makes two sends for ONE swap safe:
/// while a swap holds a number nothing has signed for, it is refused another. Without it
/// two calls that interleave at the signature both allocate, the second's `TxSigned` is
/// refused because the first attempt is open, and its number is left with no transaction.
#[test]
fn one_swap_never_holds_two_nonces_at_once() {
    let qh = swap_id(1);
    let burn = TxPurpose::Burn(qh);
    let mut state = fold(vec![funds(1), created(burn, BASE, 0)]);
    assert_eq!(
        state.check(&created(burn, BASE, 1)),
        Err(TransitionError::NonceStillUnsigned(qh)),
        "the swap is still holding nonce 0"
    );
    assert_eq!(
        state.check(&created(TxPurpose::Payout(qh), ARBITRUM, 0)),
        Err(TransitionError::NonceStillUnsigned(qh)),
        "and on any chain, because the swap is what holds the number"
    );

    // the signed record spends it, and then the swap may hold another
    let event = next_event(&state, 1, signed(qh, 1));
    apply_state_transition(&mut state, &event);
    assert!(state.unsigned_nonces().is_empty());
    let event = next_event(&state, 1, confirmed(qh, 1));
    apply_state_transition(&mut state, &event);
    assert_eq!(state.check(&created(burn, BASE, 1)), Ok(()));
}

/// The rest of `TxSigned`'s rules, checked where the number is handed out: a swap with an
/// attempt still open can never sign the transaction a creation would buy, so creating one
/// would strand the number it allocated.
#[test]
fn a_transaction_is_not_created_for_a_swap_whose_attempt_is_still_open() {
    let qh = swap_id(1);
    let state = fold(vec![
        funds(1),
        created(TxPurpose::Burn(qh), BASE, 0),
        signed(qh, 1),
    ]);
    assert_eq!(
        state.check(&created(TxPurpose::Payout(qh), BASE, 1)),
        Err(TransitionError::AttemptStillOpen(Attempt::FIRST))
    );
}

/// Rule A5, the half the fold carries: a number leaves the allocator held as unsigned, and
/// exactly two lines end that, the signed record of the transaction it was handed out for
/// and the cancel that spends it instead. A number no line ends is a gap the account can
/// never mine past, so the fold holds it where a pass can find it.
#[test]
fn a_created_nonce_is_unsigned_until_a_signed_record_or_a_cancel_ends_it() {
    let qh = swap_id(1);
    let key = NonceKey {
        chain_id: BASE,
        nonce: Nonce::ZERO,
    };
    let mut state = HeapState::default();
    for (at, payload) in [(1, funds(1)), (7, created(TxPurpose::Burn(qh), BASE, 0))] {
        state.check(&payload).expect("guard admits");
        let event = next_event(&state, at, payload);
        apply_state_transition(&mut state, &event);
    }
    assert_eq!(
        state.unsigned_nonces(),
        vec![(
            key,
            UnsignedTx {
                purpose: TxPurpose::Burn(qh),
                created_at: Timestamp::from_nanos(7),
            }
        )],
        "the allocation is stamped with the instant the log sealed it, so a replay of the \
         log rebuilds the same wait"
    );

    let mut cancelled = state.clone();
    let event = next_event(&state, 9, signed(qh, 1));
    apply_state_transition(&mut state, &event);
    assert!(
        state.unsigned_nonces().is_empty(),
        "the signed record spends the number the swap was holding"
    );

    // the other way it ends, on a state where the signature never came
    let cancel = EventType::TxCancelled {
        chain_id: BASE,
        nonce: Nonce::ZERO,
        tx_hash: TxHash::new([9; 32]),
        raw_tx: vec![0x02, 0xf8, 0x6c],
    };
    assert_eq!(cancelled.check(&cancel), Ok(()));
    let event = next_event(&cancelled, 11, cancel);
    apply_state_transition(&mut cancelled, &event);
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
    let qh = swap_id(1);
    let cancel = |nonce: u64| EventType::TxCancelled {
        chain_id: BASE,
        nonce: Nonce::new(nonce),
        tx_hash: TxHash::new([9; 32]),
        raw_tx: vec![0x02, 0xf8, 0x6c],
    };
    let never_allocated = fold(vec![funds(1)]);
    assert_eq!(
        never_allocated.check(&cancel(0)),
        Err(TransitionError::NonceNotUnsigned {
            chain_id: BASE,
            nonce: Nonce::ZERO,
        })
    );

    let signed_for = fold(vec![
        funds(1),
        created(TxPurpose::Burn(qh), BASE, 0),
        signed(qh, 1),
    ]);
    assert_eq!(
        signed_for.check(&cancel(0)),
        Err(TransitionError::NonceNotUnsigned {
            chain_id: BASE,
            nonce: Nonce::ZERO,
        }),
        "a signed transaction is out there carrying this number"
    );
    assert_eq!(
        signed_for.check(&cancel(1)),
        Err(TransitionError::NonceNotUnsigned {
            chain_id: BASE,
            nonce: Nonce::new(1),
        })
    );
}

/// Folding the log rebuilds the numbers still waiting for a signature exactly, which is
/// what lets the deep audit compare them: an allocation the log holds and the fold does not
/// would be a nonce nothing ever ends.
#[test]
fn a_replay_rebuilds_the_nonces_that_are_still_unsigned() {
    let mut state = HeapState::default();
    let mut log = Vec::new();
    for (at, payload) in [
        (1, funds(1)),
        (2, funds(2)),
        (3, created(TxPurpose::Burn(swap_id(1)), BASE, 0)),
        (4, created(TxPurpose::Payout(swap_id(2)), BASE, 1)),
        (5, signed(swap_id(2), 1)),
    ] {
        state.check(&payload).expect("guard admits");
        let event = next_event(&state, at, payload);
        apply_state_transition(&mut state, &event);
        log.push(event);
    }
    assert_eq!(
        state.unsigned_nonces().len(),
        1,
        "swap 1 is still holding its number and swap 2 signed for its own"
    );
    assert_eq!(replay(log), Ok(state));
}

/// An allocator with no successor to move to is a different refusal from a number that is
/// not the one expected: reporting it as the second would say the number is not the number
/// it is.
#[test]
fn an_allocator_out_of_numbers_says_so() {
    let mut store = fold(vec![funds(1)]).into_store();
    store.put_next_nonce(BASE, Nonce::new(u64::MAX));
    let state = State::new(store);
    assert_eq!(
        state.check(&created(TxPurpose::Burn(swap_id(1)), BASE, u64::MAX)),
        Err(TransitionError::NonceExhausted { chain_id: BASE })
    );
}

/// A transaction is created for a swap that can still send one: a swap that never received
/// funds, or that is closed, or that is waiting for its user, would strand the nonce it
/// allocated.
#[test]
fn a_transaction_is_created_only_for_a_swap_that_can_send_one() {
    let qh = swap_id(1);
    let burn = TxPurpose::Burn(qh);
    let empty = HeapState::default();
    assert_eq!(
        empty.check(&created(burn, BASE, 0)),
        Err(TransitionError::UnknownSwap(qh))
    );

    let done = fold(vec![
        funds(1),
        created(burn, BASE, 0),
        signed(qh, 1),
        confirmed(qh, 1),
        EventType::SwapDone { quote_hash: qh },
    ]);
    assert_eq!(
        done.check(&created(burn, BASE, 1)),
        Err(TransitionError::SwapClosed(SwapStatus::Done))
    );

    let waiting = fold(vec![
        funds(1),
        EventType::DecisionRequired {
            quote_hash: qh,
            reason: "slippage".into(),
        },
    ]);
    assert_eq!(
        waiting.check(&created(burn, BASE, 0)),
        Err(TransitionError::WaitingForUser)
    );

    // a cancel names no swap, so no swap has to exist for one
    assert_eq!(
        empty.check(&created(TxPurpose::Cancel(BASE), BASE, 0)),
        Ok(())
    );
}

/// A replacement keeps the nonce of the transaction it replaces: it must name one already
/// allocated on that chain, and it allocates nothing itself.
#[test]
fn a_replacement_keeps_an_allocated_nonce_and_allocates_none() {
    let qh = swap_id(1);
    let burn = TxPurpose::Burn(qh);
    let state = fold(vec![
        funds(1),
        created(burn, BASE, 0),
        signed(qh, 1),
        replaced(burn, BASE, 0),
    ]);
    assert_eq!(
        state.next_nonce(&BASE),
        Nonce::new(1),
        "a replacement allocates nothing"
    );
    assert_eq!(
        swap(&state, qh).open_attempt,
        Some(Attempt::FIRST),
        "the attempt the replacement re-sent is still open"
    );

    for never_allocated in [1, 2, u64::MAX] {
        assert_eq!(
            state.check(&replaced(burn, BASE, never_allocated)),
            Err(TransitionError::NonceNeverAllocated {
                chain_id: BASE,
                nonce: Nonce::new(never_allocated),
                next: Nonce::new(1),
            })
        );
    }
}

/// A replacement is a re-send of something in flight, so there has to be something in
/// flight: a swap with no open attempt has nothing to replace.
#[test]
fn a_replacement_needs_an_open_attempt_to_replace() {
    let qh = swap_id(1);
    let burn = TxPurpose::Burn(qh);
    let state = fold(vec![funds(1), created(burn, BASE, 0)]);
    assert_eq!(
        state.check(&replaced(burn, BASE, 0)),
        Err(TransitionError::NoOpenAttempt(qh))
    );
}

/// A value, a gas limit or a fee no sixteen-byte field holds has no preimage, so it is
/// refused by the guard rather than failing at the seal.
#[test]
fn a_transaction_field_above_the_preimage_range_is_refused() {
    let qh = swap_id(1);
    let state = fold(vec![funds(1)]);
    let field = |value: Wei, gas_limit: GasAmount, max_fee: WeiPerGas, tip: WeiPerGas| {
        EventType::TxCreated {
            purpose: TxPurpose::Burn(qh),
            chain_id: BASE,
            nonce: Nonce::ZERO,
            to: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"
                .parse()
                .unwrap(),
            value,
            data: vec![],
            gas_limit,
            max_fee,
            max_priority_fee: tip,
        }
    };
    let (gas, fee) = (GasAmount::from(120_000_u32), WeiPerGas::from(1_u8));
    assert_eq!(state.check(&field(Wei::ZERO, gas, fee, fee)), Ok(()));
    for broken in [
        field(Wei::MAX, gas, fee, fee),
        field(Wei::ZERO, GasAmount::MAX, fee, fee),
        field(Wei::ZERO, gas, WeiPerGas::MAX, fee),
        field(Wei::ZERO, gas, fee, WeiPerGas::MAX),
    ] {
        assert!(
            matches!(
                state.check(&broken),
                Err(TransitionError::AmountOutOfRange(_))
            ),
            "a field above the range was admitted"
        );
    }
}

/// The allocation a signed record spends: the number the next `TxSigned` for this swap
/// carries, handed out on Base at `nonce`. Every attempt in these fixtures is created
/// before it is signed, the way `create_and_send` does it, because a signed record for a
/// swap holding no number is refused.
fn allocated(qh: QuoteHash, nonce: u64) -> EventType {
    created(TxPurpose::Payout(qh), BASE, nonce)
}

/// The race rule A5's cancel opens: the signature is awaited across consensus rounds on
/// another subnet, and a cancel that spent the number meanwhile has sealed it. The signed
/// record that comes back late finds the swap holding nothing, and is refused: without
/// this it would be admitted and its outbox entry would overwrite the cancel's, so two
/// signed transactions would carry one nonce. The same refusal meets a signed record for a
/// swap that never allocated, and one naming a chain the swap holds no number on.
#[test]
fn a_cancelled_nonce_refuses_the_signature_that_comes_back_late() {
    let qh = swap_id(1);
    let refused = |chain: ChainId| TransitionError::NoUnsignedNonce {
        quote_hash: qh,
        chain_id: chain,
    };
    let mut state = HeapState::default();
    let mut log = Vec::new();
    for (at, payload) in [
        (1, funds(1)),
        (2, allocated(qh, 0)),
        (
            3,
            EventType::TxCancelled {
                chain_id: BASE,
                nonce: Nonce::ZERO,
                tx_hash: TxHash::new([9; 32]),
                raw_tx: vec![0x02, 0xf8, 0x6c],
            },
        ),
    ] {
        state.check(&payload).expect("guard admits");
        let event = next_event(&state, at, payload);
        apply_state_transition(&mut state, &event);
        log.push(event);
    }
    assert!(state.unsigned_nonces().is_empty(), "the cancel spent it");
    assert_eq!(
        state.check(&signed(qh, 1)),
        Err(refused(BASE)),
        "the number this signature was made for is gone"
    );
    assert_eq!(replay(log), Ok(state.clone()), "the log folds clean");

    // the swap may send again: it allocates the next number, and signs against that one
    for payload in [allocated(qh, 1), signed(qh, 1)] {
        state.check(&payload).expect("guard admits");
        let event = next_event(&state, 4, payload);
        apply_state_transition(&mut state, &event);
    }
    assert_eq!(swap(&state, qh).open_attempt, Some(Attempt::FIRST));
    assert_eq!(state.next_nonce(&BASE), Nonce::new(2));

    // a swap that never allocated holds nothing to sign against either
    let never = fold(vec![funds(1)]);
    assert_eq!(never.check(&signed(qh, 1)), Err(refused(BASE)));

    // and the number it holds is on one chain: a record naming another finds nothing
    let elsewhere = fold(vec![funds(1), allocated(qh, 0)]);
    assert_eq!(elsewhere.check(&signed(qh, 1)), Ok(()));
    let on_arbitrum = EventType::TxSigned {
        quote_hash: qh,
        attempt: Attempt::FIRST,
        chain_id: ARBITRUM,
        tx_hash: TxHash::new([1; 32]),
        raw_tx: vec![],
    };
    assert_eq!(elsewhere.check(&on_arbitrum), Err(refused(ARBITRUM)));
}

fn pull_signed(qh: QuoteHash, nonce: u64) -> EventType {
    EventType::PullSigned {
        quote_hash: qh,
        chain_id: BASE,
        nonce: Nonce::new(nonce),
        tx_hash: TxHash::new([0x33; 32]),
        raw_tx: vec![0x02, 0xf8, 0x6d],
    }
}

/// A gasless pull is a transaction with no swap behind it: it makes the deposit the claim
/// then verifies. So its allocation needs no swap, its signed record is its own line, and
/// that line spends the number the way `TxSigned` spends a swap's, so a replay rebuilds
/// the same allocator with nothing left waiting.
#[test]
fn a_pull_is_created_with_no_swap_and_its_record_spends_its_nonce() {
    let qh = swap_id(1);
    let pull = TxPurpose::GaslessPull(qh);
    let mut state = HeapState::default();
    let allocation = created(pull, BASE, 0);
    assert_eq!(
        state.check(&allocation),
        Ok(()),
        "no swap exists, and none is needed"
    );
    let event = next_event(&state, 1, allocation.clone());
    apply_state_transition(&mut state, &event);
    assert_eq!(
        state.unsigned_nonces(),
        vec![(
            NonceKey {
                chain_id: BASE,
                nonce: Nonce::ZERO
            },
            UnsignedTx {
                purpose: pull,
                created_at: Timestamp::from_nanos(1)
            }
        )]
    );
    assert_eq!(
        state.check(&signed(qh, 1)),
        Err(TransitionError::UnknownSwap(qh)),
        "a swap's signed record is not a pull's: there is no swap"
    );
    assert_eq!(state.check(&pull_signed(qh, 0)), Ok(()));
    let event = next_event(&state, 2, pull_signed(qh, 0));
    apply_state_transition(&mut state, &event);
    assert!(
        state.unsigned_nonces().is_empty(),
        "the record spent the number"
    );
    assert_eq!(state.next_nonce(&BASE), Nonce::new(1));
    assert!(
        state.swap(&qh).is_err(),
        "a pull creates no swap: the deposit it makes is what the claim verifies"
    );

    // a stuck pull is replaced at its nonce like any transaction, with no swap to ask
    assert_eq!(state.check(&replaced(pull, BASE, 0)), Ok(()));

    // and a replay of the two lines rebuilds the same allocator with nothing waiting
    let replayed = fold(vec![allocation, pull_signed(qh, 0)]);
    assert_eq!(replayed.next_nonce(&BASE), Nonce::new(1));
    assert!(replayed.unsigned_nonces().is_empty());
    assert!(replayed.swap(&qh).is_err());
}

/// A pull is for a quote nobody has paid yet: once the swap exists the funds are in, and
/// while the quote holds a number unsigned a second pull would strand one (rule A4 from
/// the quote's side, exactly as for a swap).
#[test]
fn a_pull_is_refused_once_the_swap_exists_or_while_the_quote_holds_a_nonce() {
    let qh = swap_id(1);
    let pull = TxPurpose::GaslessPull(qh);
    let paid = fold(vec![funds(1)]);
    assert_eq!(
        paid.check(&created(pull, BASE, 0)),
        Err(TransitionError::SwapExists(qh)),
        "the deposit is already in"
    );
    let holding = fold(vec![created(pull, BASE, 0)]);
    assert_eq!(
        holding.check(&created(pull, BASE, 1)),
        Err(TransitionError::NonceStillUnsigned(qh)),
        "one number at a time per quote"
    );
    // and a closed or waiting swap is still refused a pull, through the same door
    let done = fold(vec![
        funds(1),
        EventType::Frozen {
            quote_hash: qh,
            reason: "sanctions".into(),
        },
    ]);
    assert!(matches!(
        done.check(&created(pull, BASE, 0)),
        Err(TransitionError::SwapExists(_))
    ));
}

/// The pull's record spends the number that was handed out for that pull and nothing else:
/// not a number nobody allocated, not a swap's number, not one a cancel already spent, and
/// not another quote's pull.
#[test]
fn a_pull_record_spends_only_the_nonce_held_for_that_pull() {
    let qh = swap_id(1);
    let other = swap_id(2);
    let refused = |nonce: u64| TransitionError::NonceNotHeldForPull {
        chain_id: BASE,
        nonce: Nonce::new(nonce),
        quote_hash: qh,
    };
    let nothing = HeapState::default();
    assert_eq!(nothing.check(&pull_signed(qh, 0)), Err(refused(0)));

    let swaps_number = fold(vec![funds(1), allocated(qh, 0)]);
    assert_eq!(
        swaps_number.check(&pull_signed(qh, 0)),
        Err(refused(0)),
        "a swap's allocation is spent by TxSigned, not by a pull's record"
    );

    let another_pull = fold(vec![created(TxPurpose::GaslessPull(other), BASE, 0)]);
    assert_eq!(another_pull.check(&pull_signed(qh, 0)), Err(refused(0)));

    let cancelled = fold(vec![
        created(TxPurpose::GaslessPull(qh), BASE, 0),
        EventType::TxCancelled {
            chain_id: BASE,
            nonce: Nonce::ZERO,
            tx_hash: TxHash::new([9; 32]),
            raw_tx: vec![0x02, 0xf8, 0x6c],
        },
    ]);
    assert_eq!(
        cancelled.check(&pull_signed(qh, 0)),
        Err(refused(0)),
        "the late record of a cancelled pull is refused, as a swap's is"
    );
    assert_eq!(
        cancelled.check(&pull_signed(qh, 1)),
        Err(refused(1)),
        "and a number never handed out is not one to spend"
    );
}

/// The engine reads where a swap is from its latest leg and how it ended, and the fold
/// learns both from lines it already writes: the leg from the purpose of the number the
/// signed record spends, the outcome from the line that closes the attempt. A replay
/// rebuilds the same pair.
/// The fold keeps the hash of the transaction the latest attempt confirmed as: what binds
/// a pushed attestation to the swap's own burn and what the mint read is looked up by. It
/// is set by the confirming line, cleared by the next signed record (a new attempt), and
/// absent after a failure, and a replay rebuilds it.
#[test]
fn the_fold_keeps_the_hash_the_latest_attempt_confirmed_as() {
    let qh = swap_id(1);
    let mut state = fold(vec![funds(1)]);
    let hash_of = |state: &HeapState| swap(state, qh).last_tx_hash;
    let step = |state: &mut HeapState, payload: EventType| {
        state.check(&payload).expect("guard admits");
        let event = next_event(state, 1, payload);
        apply_state_transition(state, &event);
    };
    assert_eq!(hash_of(&state), None, "nothing confirmed yet");
    step(&mut state, created(TxPurpose::Burn(qh), BASE, 0));
    step(&mut state, signed(qh, 1));
    assert_eq!(hash_of(&state), None, "signed is not confirmed");
    step(&mut state, confirmed(qh, 1));
    assert_eq!(
        hash_of(&state),
        Some(TxHash::new([1; 32])),
        "the confirming line's hash"
    );
    step(&mut state, created(TxPurpose::Mint(qh), BASE, 1));
    step(&mut state, signed(qh, 2));
    assert_eq!(
        hash_of(&state),
        None,
        "a new attempt has not confirmed as anything yet"
    );
    step(&mut state, failed(qh, 2));
    assert_eq!(
        hash_of(&state),
        None,
        "a failed attempt confirmed as nothing"
    );
    let replayed = fold(vec![
        funds(1),
        created(TxPurpose::Burn(qh), BASE, 0),
        signed(qh, 1),
        confirmed(qh, 1),
    ]);
    assert_eq!(hash_of(&replayed), Some(TxHash::new([1; 32])));
}

#[test]
fn the_fold_knows_the_latest_leg_and_how_it_ended() {
    use types::{Leg, Outcome};
    let qh = swap_id(1);
    let mut state = fold(vec![funds(1)]);
    let progress = |state: &HeapState| {
        let swap = swap(state, qh);
        (swap.last_leg, swap.last_outcome)
    };
    assert_eq!(progress(&state), (None, None), "nothing signed yet");

    let step = |state: &mut HeapState, payload: EventType| {
        state.check(&payload).expect("guard admits");
        let event = next_event(state, 1, payload);
        apply_state_transition(state, &event);
    };
    step(&mut state, created(TxPurpose::Burn(qh), BASE, 0));
    assert_eq!(
        progress(&state),
        (None, None),
        "an allocation is not a leg yet"
    );
    step(&mut state, signed(qh, 1));
    assert_eq!(
        progress(&state),
        (Some(Leg::Burn), None),
        "signed: the leg is known and it is open"
    );
    step(&mut state, confirmed(qh, 1));
    assert_eq!(
        progress(&state),
        (Some(Leg::Burn), Some(Outcome::Confirmed))
    );

    step(&mut state, created(TxPurpose::Mint(qh), BASE, 1));
    step(&mut state, signed(qh, 2));
    assert_eq!(
        progress(&state),
        (Some(Leg::Mint), None),
        "a new leg supersedes the old outcome"
    );
    step(&mut state, failed(qh, 2));
    assert_eq!(progress(&state), (Some(Leg::Mint), Some(Outcome::Failed)));

    let replayed = fold(vec![
        funds(1),
        created(TxPurpose::Burn(qh), BASE, 0),
        signed(qh, 1),
        confirmed(qh, 1),
        created(TxPurpose::Mint(qh), BASE, 1),
        signed(qh, 2),
        failed(qh, 2),
    ]);
    assert_eq!(
        progress(&replayed),
        (Some(Leg::Mint), Some(Outcome::Failed))
    );

    // a reclaim is a swap's leg like the others
    let mut reclaimed = fold(vec![funds(2)]);
    step(
        &mut reclaimed,
        created(TxPurpose::Reclaim(swap_id(2)), BASE, 0),
    );
    step(&mut reclaimed, signed(swap_id(2), 1));
    assert_eq!(swap(&reclaimed, swap_id(2)).last_leg, Some(Leg::Reclaim));
}

/// A CCTP burn's `TxCreated` carries the burn's calldata, and the fold reads the most it
/// offered Circle off it (rule A3): the message the burn emits carries that fee, so the
/// binding of its attestation reads it from here and never from a minimum fee the config
/// may have moved to since. A burn-purpose line whose calldata is no CCTP burn (the Eco
/// publish, which also leaves the source vault as the swap's `Burn` leg) records none.
#[test]
fn a_cctp_burn_records_the_fee_it_offered_and_a_publish_records_none() {
    use types::abi::{cctp_deposit_for_burn_with_hook, vault_execute, Burn, VaultCall};
    let qh = swap_id(1);
    let usdc: types::EvmAddress = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"
        .parse()
        .unwrap();
    let burn = Burn {
        amount: amount(25_000_000),
        destination_domain: 3,
        mint_recipient: [0x22; 32],
        burn_token: usdc,
        destination_caller: [0x75; 32],
        max_fee: amount(12_500),
        min_finality_threshold: 2_000,
        hook_data: qh.into_bytes().to_vec(),
    };
    let execute = vault_execute(
        qh,
        &[VaultCall {
            target: "0x28b5a0e9C621a5BadaA536219b3a228C8168cf5d"
                .parse()
                .unwrap(),
            value: Wei::ZERO,
            data: cctp_deposit_for_burn_with_hook(&burn),
            approve_token: usdc,
            approve_amount: amount(25_000_000),
        }],
        &[],
    );
    let burned = |data: Vec<u8>| match created(TxPurpose::Burn(qh), BASE, 0) {
        EventType::TxCreated {
            purpose,
            chain_id,
            nonce,
            to,
            value,
            gas_limit,
            max_fee,
            max_priority_fee,
            ..
        } => EventType::TxCreated {
            purpose,
            chain_id,
            nonce,
            to,
            value,
            data,
            gas_limit,
            max_fee,
            max_priority_fee,
        },
        other => panic!("the fixture is a TxCreated, not {other:?}"),
    };
    let state = fold(vec![funds(1), burned(execute)]);
    assert_eq!(swap(&state, qh).burn_max_fee, Some(amount(12_500)));
    let state = fold(vec![funds(1), burned(vec![0xde, 0xad, 0xbe, 0xef])]);
    assert_eq!(
        swap(&state, qh).burn_max_fee,
        None,
        "calldata that is no CCTP burn records no fee"
    );
    // an Eco publish is a vault execute like the burn, of the Portal's publishAndFund
    // rather than the messenger's burn: the execute decodes, the burn inside it does not
    let publish = vault_execute(
        qh,
        &[VaultCall {
            target: "0xEC000064576f9C95a8623Bc0eff3db6d296ea6df"
                .parse()
                .unwrap(),
            value: Wei::ZERO,
            data: types::abi::eco_publish_and_fund(
                ARBITRUM,
                &[0xde, 0xad],
                &types::abi::EcoReward {
                    deadline: UnixSeconds::new(1_800_000_600),
                    creator: "0x1111111111111111111111111111111111111111"
                        .parse()
                        .unwrap(),
                    prover: "0xeC00008537c1F26E739486BCFCC818d81234d5aD"
                        .parse()
                        .unwrap(),
                    native_amount: Wei::ZERO,
                    tokens: vec![(usdc, amount(25_000_000))],
                },
            ),
            approve_token: usdc,
            approve_amount: amount(25_000_000),
        }],
        &[],
    );
    assert!(types::abi::decode_vault_execute(&publish).is_some());
    let state = fold(vec![funds(1), burned(publish)]);
    assert_eq!(
        swap(&state, qh).burn_max_fee,
        None,
        "an Eco publish rides the burn leg and records no fee"
    );
}

/// One fee line per swap is a rule of the fold (rule A6), not only a habit of its one
/// writer: the second `FeeAccrued` for a swap that already holds its fee is refused with
/// the swap named, so no path, a retried `record_done` or a hand-written line, can accrue a
/// swap's fee twice. Another swap's fee still lands.
#[test]
fn a_second_fee_line_for_one_swap_is_refused() {
    let qh = swap_id(1);
    let fee = |qh, units| EventType::FeeAccrued {
        quote_hash: qh,
        amount: amount(units),
    };
    let state = fold(vec![funds(1), funds(2), fee(qh, 7)]);
    assert_eq!(swap(&state, qh).fee_accrued, Some(amount(7)));
    assert_eq!(
        state.check(&fee(qh, 3)),
        Err(TransitionError::FeeAlreadyAccrued(qh))
    );
    assert_eq!(state.check(&fee(swap_id(2), 3)), Ok(()));
}
