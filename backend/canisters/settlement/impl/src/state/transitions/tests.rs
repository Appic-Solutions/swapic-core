use super::*;
use types::events::Choice;
use types::{
    Attempt, BlockNumber, ChainId, EventHash, EventIndex, LedgerMeta, Pocket, PocketError,
    QuoteHash, Swap, SwapStatus, Timestamp, TokenAmount, TxHash,
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

fn funds(qh: QuoteHash) -> EventType {
    EventType::FundsReceived {
        quote_hash: qh,
        quote_bytes: vec![1],
        chain_id: BASE,
        token: "usdc".parse().unwrap(),
        amount: amount(100),
        tx_ref: "0xabc".into(),
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
    let qh = qh(1);
    let state = fold(vec![
        funds(qh),
        signed(qh, 1),
        confirmed(qh, 1),
        EventType::PaidInStable {
            quote_hash: qh,
            chain_id: ARBITRUM,
            amount: amount(99),
        },
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
    let qh = qh(1);
    let mut state = fold(vec![funds(qh), signed(qh, 1)]);
    assert_eq!(
        state.check(&signed(qh, 2)),
        Err(TransitionError::AttemptStillOpen(Attempt::FIRST))
    );
    // closing attempt 1 unblocks attempt 2
    let env = next_event(&state, 1, confirmed(qh, 1));
    apply_state_transition(&mut state, &env);
    assert!(state.check(&signed(qh, 2)).is_ok());
}

#[test]
fn attempt_numbers_are_strictly_sequential() {
    let state = fold(vec![funds(qh(1))]);
    // skips 1
    assert_eq!(
        state.check(&signed(qh(1), 2)),
        Err(TransitionError::AttemptOutOfSequence {
            attempt: Attempt::new(2),
            expected: Some(Attempt::FIRST)
        })
    );
}

#[test]
fn closed_swap_rejects_new_signatures() {
    let qh = qh(1);
    let state = fold(vec![
        funds(qh),
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
    let state = fold(vec![funds(qh(1))]);
    assert_eq!(
        state.check(&funds(qh(1))),
        Err(TransitionError::SwapExists(qh(1)))
    );
}

#[test]
fn replay_is_deterministic() {
    let qh = qh(1);
    let events: Vec<Event> = {
        let mut state = HeapState::default();
        let mut out = vec![];
        for e in vec![funds(qh), signed(qh, 1), confirmed(qh, 1)] {
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
        funds(qh(1)),
        EventType::PocketReserved {
            quote_hash: qh(1),
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
    let qh = qh(1);
    let mut events = vec![funds(qh), signed(qh, 1), confirmed(qh, 1), paid(qh, 99)];
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
    let qh = qh(1);
    let mut events = vec![funds(qh), signed(qh, 1), confirmed(qh, 1), paid(qh, 0)];
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
    let qh = qh(1);
    let mut state = fold(vec![funds(qh), signed(qh, 1), confirmed(qh, 1), ask(qh)]);
    assert_eq!(
        state.check(&signed(qh, 2)),
        Err(TransitionError::WaitingForUser)
    );
    // answering the question unblocks the next attempt
    let resume = EventType::DecisionMade {
        quote_hash: qh,
        choice: Choice::Requote,
    };
    let env = next_event(&state, 1, resume);
    apply_state_transition(&mut state, &env);
    assert!(state.check(&signed(qh, 2)).is_ok());
}

#[test]
fn done_swap_rejects_freeze() {
    let qh = qh(1);
    let state = fold(vec![
        funds(qh),
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
    let qh = qh(1);
    let state = fold(vec![funds(qh), signed(qh, 1), failed(qh, 1)]);
    assert_eq!(swap(&state, qh).open_attempt, None);
    assert_eq!(swap(&state, qh).last_attempt, Some(Attempt::FIRST));
    // a failed attempt still counts, so the retry is number 2
    assert!(matches!(
        state.check(&signed(qh, 1)),
        Err(TransitionError::AttemptOutOfSequence { .. })
    ));
    assert!(state.check(&signed(qh, 2)).is_ok());
}

#[test]
fn refund_path_reaches_refunded() {
    let qh = qh(1);
    let state = fold(vec![
        funds(qh),
        EventType::RefundStarted {
            quote_hash: qh,
            reason: "timeout".into(),
        },
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
    let qh = qh(1);
    let state = fold(vec![
        EventType::PocketFunded {
            chain_id: BASE,
            amount: amount(1000),
        },
        funds(qh),
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
    let qh = qh(1);
    let state = fold(vec![
        funds(qh),
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
    let qh = qh(1);
    let state = fold(vec![
        funds(qh),
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
    let qh = qh(1);
    let state = fold(vec![funds(qh), ask(qh)]);
    // re-asking would silently re-arm the deadline
    assert_eq!(state.check(&ask(qh)), Err(TransitionError::WaitingForUser));
}

#[test]
fn decision_to_refund_reaches_refunding() {
    let qh = qh(1);
    let mut events = vec![funds(qh)];
    events.extend(decide(qh, Choice::Refund));
    let state = fold(events);
    assert_eq!(swap(&state, qh).status, SwapStatus::Refunding);
    assert_eq!(swap(&state, qh).waiting_since, None);
}

// Unlike a release, a spend does not return the value: the pocket total drops.
#[test]
fn pocket_spend_debits_reserved_without_returning_it() {
    let qh = qh(1);
    let state = fold(vec![
        EventType::PocketFunded {
            chain_id: BASE,
            amount: amount(1000),
        },
        funds(qh),
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
    let qh = qh(1);
    let mut state = fold(vec![funds(qh)]);
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
    let qh = qh(1);
    let mut store = MemoryStore::default();
    store.put_swap(qh, swap(&fold(vec![funds(qh)]), qh));
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
    let qh = qh(1);
    let mut state = HeapState::default();
    let mut log = vec![];
    for payload in [
        EventType::PocketFunded {
            chain_id: BASE,
            amount: amount(1000),
        },
        funds(qh),
        signed(qh, 1),
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
    let qh = qh(1);
    let mut state = HeapState::default();
    let mut log = vec![];
    for payload in [funds(qh), signed(qh, 1)] {
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
            index: EventIndex::new(2),
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
