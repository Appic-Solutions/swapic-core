use super::*;
use types::{BlockNumber, Timestamp, TxHash};

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
fn next_event(state: &AppState, nanos: u64, payload: EventType) -> Event {
    Event::seal(
        state.next_event_index,
        Timestamp::from_nanos(nanos),
        state.last_event_hash,
        payload,
    )
}

fn fold(events: Vec<EventType>) -> AppState {
    let mut state = AppState::default();
    for e in events {
        check_transition(&state, &e).expect("guard admits");
        let event = next_event(&state, 1, e);
        apply(&mut state, &event);
    }
    state
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
    let swap = &state.swaps[&qh];
    assert_eq!(swap.status, SwapStatus::Done);
    assert_eq!(swap.last_attempt, Some(Attempt::new(2)));
    assert_eq!(swap.amount_paid, amount(99));
}

#[test]
fn cannot_sign_next_attempt_while_one_is_open() {
    let qh = qh(1);
    let mut state = fold(vec![funds(qh), signed(qh, 1)]);
    assert!(check_transition(&state, &signed(qh, 2)).is_err());
    // closing attempt 1 unblocks attempt 2
    let env = next_event(&state, 1, confirmed(qh, 1));
    apply(&mut state, &env);
    assert!(check_transition(&state, &signed(qh, 2)).is_ok());
}

#[test]
fn attempt_numbers_are_strictly_sequential() {
    let state = fold(vec![funds(qh(1))]);
    assert!(check_transition(&state, &signed(qh(1), 2)).is_err()); // skips 1
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
    assert!(check_transition(&state, &signed(qh, 1)).is_err());
}

#[test]
fn duplicate_funds_received_rejected() {
    let state = fold(vec![funds(qh(1))]);
    assert!(check_transition(&state, &funds(qh(1))).is_err());
}

#[test]
fn replay_is_deterministic() {
    let qh = qh(1);
    let events: Vec<Event> = {
        let mut state = AppState::default();
        let mut out = vec![];
        for e in vec![funds(qh), signed(qh, 1), confirmed(qh, 1)] {
            let env = next_event(&state, 7, e);
            apply(&mut state, &env);
            out.push(env);
        }
        out
    };
    assert_eq!(
        replay(events.clone().into_iter()),
        replay(events.into_iter())
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
    assert_eq!(state.pockets[&BASE].available, amount(600));
    assert_eq!(state.pockets[&BASE].reserved, amount(400));
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
    assert_eq!(state.swaps[&qh].status, SwapStatus::Executing);
    assert!(check_transition(&state, &paid(qh, 50)).is_err());
    assert_eq!(state.swaps[&qh].amount_paid, amount(99));
}

#[test]
fn waiting_for_user_blocks_signing() {
    let qh = qh(1);
    let mut state = fold(vec![funds(qh), signed(qh, 1), confirmed(qh, 1), ask(qh)]);
    assert!(check_transition(&state, &signed(qh, 2)).is_err());
    // answering the question unblocks the next attempt
    let resume = EventType::DecisionMade {
        quote_hash: qh,
        choice: Choice::Requote,
    };
    let env = next_event(&state, 1, resume);
    apply(&mut state, &env);
    assert!(check_transition(&state, &signed(qh, 2)).is_ok());
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
    assert_eq!(state.swaps[&qh].status, SwapStatus::Done);
    assert!(check_transition(
        &state,
        &EventType::Frozen {
            quote_hash: qh,
            reason: "sanctions".into(),
        }
    )
    .is_err());
}

#[test]
fn fee_needs_a_swap_but_config_and_funding_do_not() {
    let state = AppState::default();
    assert!(check_transition(
        &state,
        &EventType::FeeAccrued {
            quote_hash: qh(9),
            amount: amount(7),
        }
    )
    .is_err());
    assert!(check_transition(&state, &EventType::ConfigChanged { json: "{}".into() }).is_ok());
    assert!(check_transition(
        &state,
        &EventType::PocketFunded {
            chain_id: BASE,
            amount: amount(1),
        }
    )
    .is_ok());
}

#[test]
fn tx_failed_closes_the_attempt() {
    let qh = qh(1);
    let state = fold(vec![funds(qh), signed(qh, 1), failed(qh, 1)]);
    assert_eq!(state.swaps[&qh].open_attempt, None);
    assert_eq!(state.swaps[&qh].last_attempt, Some(Attempt::FIRST));
    // a failed attempt still counts, so the retry is number 2
    assert!(check_transition(&state, &signed(qh, 1)).is_err());
    assert!(check_transition(&state, &signed(qh, 2)).is_ok());
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
    assert_eq!(state.swaps[&qh].status, SwapStatus::Refunded);
    // Refunded is terminal
    assert!(check_transition(&state, &signed(qh, 2)).is_err());
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
    assert_eq!(state.pockets[&BASE].available, amount(750));
    assert_eq!(state.pockets[&BASE].reserved, amount(250));
    // cannot release more than is reserved
    assert!(check_transition(
        &state,
        &EventType::PocketReleased {
            quote_hash: qh,
            chain_id: BASE,
            amount: amount(300),
        }
    )
    .is_err());
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
    assert_eq!(state.swaps[&qh].status, SwapStatus::Refunding);
    assert_eq!(state.swaps[&qh].waiting_since, None);
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
    assert_eq!(state.swaps[&qh].status, SwapStatus::Frozen);
    assert_eq!(state.swaps[&qh].waiting_since, None);
}

#[test]
fn second_decision_request_rejected() {
    let qh = qh(1);
    let state = fold(vec![funds(qh), ask(qh)]);
    // re-asking would silently re-arm the deadline
    assert!(check_transition(&state, &ask(qh)).is_err());
}

#[test]
fn decision_to_refund_reaches_refunding() {
    let qh = qh(1);
    let mut events = vec![funds(qh)];
    events.extend(decide(qh, Choice::Refund));
    let state = fold(events);
    assert_eq!(state.swaps[&qh].status, SwapStatus::Refunding);
    assert_eq!(state.swaps[&qh].waiting_since, None);
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
    let p = &state.pockets[&BASE];
    assert_eq!(p.available, amount(600));
    assert_eq!(p.reserved, amount(150));
    assert_eq!(
        p.available.checked_add(p.reserved),
        Some(amount(750)),
        "250 left the pocket system"
    );
    // cannot spend more than is reserved
    assert!(check_transition(
        &state,
        &EventType::PocketSpent {
            quote_hash: qh,
            chain_id: BASE,
            amount: amount(200),
        }
    )
    .is_err());
}

#[test]
fn pocket_reserve_for_unknown_swap_rejected() {
    let state = fold(vec![EventType::PocketFunded {
        chain_id: BASE,
        amount: amount(1000),
    }]);
    assert!(check_transition(
        &state,
        &EventType::PocketReserved {
            quote_hash: qh(9),
            chain_id: BASE,
            amount: amount(400),
        }
    )
    .is_err());
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
    assert_eq!(state.pockets[&BASE].available, amount(750));
    assert_eq!(state.pockets[&ARBITRUM].available, amount(250));
    // the source chain cannot send what it does not have
    assert!(check_transition(
        &state,
        &EventType::PocketRebalanced {
            from_chain: BASE,
            to_chain: ARBITRUM,
            amount: amount(751),
            route: "cctp".into(),
        }
    )
    .is_err());
}

// The waiting clock is the only place apply reads the event timestamp, so seal this one
// by hand with a distinctive time instead of fold's constant 1.
#[test]
fn decision_sets_then_clears_the_waiting_clock() {
    let qh = qh(1);
    let mut state = fold(vec![funds(qh)]);
    let pause = ask(qh);
    check_transition(&state, &pause).expect("guard admits pause");
    let env = next_event(&state, 777, pause);
    apply(&mut state, &env);
    assert_eq!(state.swaps[&qh].status, SwapStatus::WaitingForUser);
    assert_eq!(
        state.swaps[&qh].waiting_since,
        Some(Timestamp::from_nanos(777))
    );

    let resume = EventType::DecisionMade {
        quote_hash: qh,
        choice: Choice::Requote,
    };
    check_transition(&state, &resume).expect("guard admits resume");
    let env = next_event(&state, 900, resume);
    apply(&mut state, &env);
    assert_eq!(state.swaps[&qh].status, SwapStatus::Executing);
    assert_eq!(state.swaps[&qh].waiting_since, None);
}
