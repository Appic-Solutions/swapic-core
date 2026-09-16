use super::*;
use settlement_api::types::events::seal;

fn funds(qh: Hash32) -> Event {
    Event::FundsReceived {
        quote_hash: qh,
        quote_bytes: vec![1],
        chain_id: 8453,
        token: "usdc".into(),
        amount: 100,
        tx_ref: "0xabc".into(),
    }
}

fn signed(qh: Hash32, attempt: u32) -> Event {
    Event::TxSigned {
        quote_hash: qh,
        attempt,
        chain_id: 8453,
        tx_hash: [attempt as u8; 32],
        raw_tx: vec![],
    }
}

fn confirmed(qh: Hash32, attempt: u32) -> Event {
    Event::TxConfirmed {
        quote_hash: qh,
        attempt,
        chain_id: 8453,
        tx_hash: [attempt as u8; 32],
        block: 1,
    }
}

fn fold(events: Vec<Event>) -> AppState {
    let mut state = AppState::default();
    for e in events {
        check_transition(&state, &e).expect("guard admits");
        let env = seal(state.next_event_index, 1, state.last_event_hash, e);
        apply(&mut state, &env);
    }
    state
}

#[test]
fn happy_path_reaches_done() {
    let qh = [1; 32];
    let state = fold(vec![
        funds(qh),
        signed(qh, 1),
        confirmed(qh, 1),
        Event::PaidInStable {
            quote_hash: qh,
            chain_id: 42161,
            amount: 99,
        },
        signed(qh, 2),
        confirmed(qh, 2),
        Event::SwapDone { quote_hash: qh },
    ]);
    let swap = &state.swaps[&qh];
    assert_eq!(swap.status, SwapStatus::Done);
    assert_eq!(swap.attempts, 2);
    assert_eq!(swap.amount_paid, 99);
}

#[test]
fn cannot_sign_next_attempt_while_one_is_open() {
    let qh = [1; 32];
    let mut state = fold(vec![funds(qh), signed(qh, 1)]);
    assert!(check_transition(&state, &signed(qh, 2)).is_err());
    // closing attempt 1 unblocks attempt 2
    let env = seal(
        state.next_event_index,
        1,
        state.last_event_hash,
        confirmed(qh, 1),
    );
    apply(&mut state, &env);
    assert!(check_transition(&state, &signed(qh, 2)).is_ok());
}

#[test]
fn attempt_numbers_are_strictly_sequential() {
    let state = fold(vec![funds([1; 32])]);
    assert!(check_transition(&state, &signed([1; 32], 2)).is_err()); // skips 1
}

#[test]
fn closed_swap_rejects_new_signatures() {
    let qh = [1; 32];
    let state = fold(vec![
        funds(qh),
        Event::Frozen {
            quote_hash: qh,
            reason: "sanctions".into(),
        },
    ]);
    assert!(check_transition(&state, &signed(qh, 1)).is_err());
}

#[test]
fn duplicate_funds_received_rejected() {
    let state = fold(vec![funds([1; 32])]);
    assert!(check_transition(&state, &funds([1; 32])).is_err());
}

#[test]
fn replay_is_deterministic() {
    let qh = [1; 32];
    let events: Vec<EventEnvelope> = {
        let mut state = AppState::default();
        let mut out = vec![];
        for e in vec![funds(qh), signed(qh, 1), confirmed(qh, 1)] {
            let env = seal(state.next_event_index, 7, state.last_event_hash, e);
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
        Event::PocketFunded {
            chain_id: 8453,
            amount: 1000,
        },
        funds([1; 32]),
        Event::PocketReserved {
            quote_hash: [1; 32],
            chain_id: 8453,
            amount: 400,
        },
    ]);
    assert_eq!(state.pockets[&8453].available, 600);
    assert_eq!(state.pockets[&8453].reserved, 400);
}

fn failed(qh: Hash32, attempt: u32) -> Event {
    Event::TxFailed {
        quote_hash: qh,
        attempt,
        reason: "reverted".into(),
    }
}

fn decide(qh: Hash32, choice: Choice) -> Vec<Event> {
    vec![
        ask(qh),
        Event::DecisionMade {
            quote_hash: qh,
            choice,
        },
    ]
}

fn paid(qh: Hash32, amount: u128) -> Event {
    Event::PaidInStable {
        quote_hash: qh,
        chain_id: 42161,
        amount,
    }
}

// A requote returns the swap to Executing, which re-opens the PaidInStable arm;
// without the amount_paid check the second event would overwrite the first amount.
#[test]
fn second_paid_in_stable_rejected() {
    let qh = [1; 32];
    let mut events = vec![funds(qh), signed(qh, 1), confirmed(qh, 1), paid(qh, 99)];
    events.extend(decide(qh, Choice::Requote));
    let state = fold(events);
    assert_eq!(state.swaps[&qh].status, SwapStatus::Executing);
    assert!(check_transition(&state, &paid(qh, 50)).is_err());
    assert_eq!(state.swaps[&qh].amount_paid, 99);
}

#[test]
fn waiting_for_user_blocks_signing() {
    let qh = [1; 32];
    let mut state = fold(vec![funds(qh), signed(qh, 1), confirmed(qh, 1), ask(qh)]);
    assert!(check_transition(&state, &signed(qh, 2)).is_err());
    // answering the question unblocks the next attempt
    let resume = Event::DecisionMade {
        quote_hash: qh,
        choice: Choice::Requote,
    };
    let env = seal(state.next_event_index, 1, state.last_event_hash, resume);
    apply(&mut state, &env);
    assert!(check_transition(&state, &signed(qh, 2)).is_ok());
}

#[test]
fn done_swap_rejects_freeze() {
    let qh = [1; 32];
    let state = fold(vec![
        funds(qh),
        signed(qh, 1),
        confirmed(qh, 1),
        Event::SwapDone { quote_hash: qh },
    ]);
    assert_eq!(state.swaps[&qh].status, SwapStatus::Done);
    assert!(check_transition(
        &state,
        &Event::Frozen {
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
        &Event::FeeAccrued {
            quote_hash: [9; 32],
            amount: 7,
        }
    )
    .is_err());
    assert!(check_transition(&state, &Event::ConfigChanged { json: "{}".into() }).is_ok());
    assert!(check_transition(
        &state,
        &Event::PocketFunded {
            chain_id: 8453,
            amount: 1,
        }
    )
    .is_ok());
}

#[test]
fn tx_failed_closes_the_attempt() {
    let qh = [1; 32];
    let state = fold(vec![funds(qh), signed(qh, 1), failed(qh, 1)]);
    assert_eq!(state.swaps[&qh].open_attempt, None);
    assert_eq!(state.swaps[&qh].attempts, 1);
    // a failed attempt still counts, so the retry is number 2
    assert!(check_transition(&state, &signed(qh, 1)).is_err());
    assert!(check_transition(&state, &signed(qh, 2)).is_ok());
}

#[test]
fn refund_path_reaches_refunded() {
    let qh = [1; 32];
    let state = fold(vec![
        funds(qh),
        Event::RefundStarted {
            quote_hash: qh,
            reason: "timeout".into(),
        },
        signed(qh, 1),
        confirmed(qh, 1),
        Event::Refunded {
            quote_hash: qh,
            chain_id: 8453,
            token: "usdc".into(),
            amount: 100,
            to: "0xuser".into(),
        },
    ]);
    assert_eq!(state.swaps[&qh].status, SwapStatus::Refunded);
    // Refunded is terminal
    assert!(check_transition(&state, &signed(qh, 2)).is_err());
}

#[test]
fn pocket_release_returns_reserved_to_available() {
    let qh = [1; 32];
    let state = fold(vec![
        Event::PocketFunded {
            chain_id: 8453,
            amount: 1000,
        },
        funds(qh),
        Event::PocketReserved {
            quote_hash: qh,
            chain_id: 8453,
            amount: 400,
        },
        Event::PocketReleased {
            quote_hash: qh,
            chain_id: 8453,
            amount: 150,
        },
    ]);
    assert_eq!(state.pockets[&8453].available, 750);
    assert_eq!(state.pockets[&8453].reserved, 250);
    // cannot release more than is reserved
    assert!(check_transition(
        &state,
        &Event::PocketReleased {
            quote_hash: qh,
            chain_id: 8453,
            amount: 300,
        }
    )
    .is_err());
}

fn ask(qh: Hash32) -> Event {
    Event::DecisionRequired {
        quote_hash: qh,
        reason: "slippage".into(),
    }
}

// A swap that stops waiting must stop the clock too, or Task 7's expiry sweep would
// keep seeing a stale deadline on a swap that is no longer waiting for anyone.
#[test]
fn refund_stops_the_waiting_clock() {
    let qh = [1; 32];
    let state = fold(vec![
        funds(qh),
        ask(qh),
        Event::RefundStarted {
            quote_hash: qh,
            reason: "timeout".into(),
        },
    ]);
    assert_eq!(state.swaps[&qh].status, SwapStatus::Refunding);
    assert_eq!(state.swaps[&qh].waiting_since_ns, None);
}

#[test]
fn freeze_stops_the_waiting_clock() {
    let qh = [1; 32];
    let state = fold(vec![
        funds(qh),
        ask(qh),
        Event::Frozen {
            quote_hash: qh,
            reason: "sanctions".into(),
        },
    ]);
    assert_eq!(state.swaps[&qh].status, SwapStatus::Frozen);
    assert_eq!(state.swaps[&qh].waiting_since_ns, None);
}

#[test]
fn second_decision_request_rejected() {
    let qh = [1; 32];
    let state = fold(vec![funds(qh), ask(qh)]);
    // re-asking would silently re-arm the deadline
    assert!(check_transition(&state, &ask(qh)).is_err());
}

#[test]
fn decision_to_refund_reaches_refunding() {
    let qh = [1; 32];
    let mut events = vec![funds(qh)];
    events.extend(decide(qh, Choice::Refund));
    let state = fold(events);
    assert_eq!(state.swaps[&qh].status, SwapStatus::Refunding);
    assert_eq!(state.swaps[&qh].waiting_since_ns, None);
}

// Unlike a release, a spend does not return the value: the pocket total drops.
#[test]
fn pocket_spend_debits_reserved_without_returning_it() {
    let qh = [1; 32];
    let state = fold(vec![
        Event::PocketFunded {
            chain_id: 8453,
            amount: 1000,
        },
        funds(qh),
        Event::PocketReserved {
            quote_hash: qh,
            chain_id: 8453,
            amount: 400,
        },
        Event::PocketSpent {
            quote_hash: qh,
            chain_id: 8453,
            amount: 250,
        },
    ]);
    let p = &state.pockets[&8453];
    assert_eq!(p.available, 600);
    assert_eq!(p.reserved, 150);
    assert_eq!(p.available + p.reserved, 750, "250 left the pocket system");
    // cannot spend more than is reserved
    assert!(check_transition(
        &state,
        &Event::PocketSpent {
            quote_hash: qh,
            chain_id: 8453,
            amount: 200,
        }
    )
    .is_err());
}

#[test]
fn pocket_reserve_for_unknown_swap_rejected() {
    let state = fold(vec![Event::PocketFunded {
        chain_id: 8453,
        amount: 1000,
    }]);
    assert!(check_transition(
        &state,
        &Event::PocketReserved {
            quote_hash: [9; 32],
            chain_id: 8453,
            amount: 400,
        }
    )
    .is_err());
}

#[test]
fn pocket_rebalance_moves_available_between_chains() {
    let state = fold(vec![
        Event::PocketFunded {
            chain_id: 8453,
            amount: 1000,
        },
        Event::PocketRebalanced {
            from_chain: 8453,
            to_chain: 42161,
            amount: 250,
            route: "cctp".into(),
        },
    ]);
    assert_eq!(state.pockets[&8453].available, 750);
    assert_eq!(state.pockets[&42161].available, 250);
    // the source chain cannot send what it does not have
    assert!(check_transition(
        &state,
        &Event::PocketRebalanced {
            from_chain: 8453,
            to_chain: 42161,
            amount: 751,
            route: "cctp".into(),
        }
    )
    .is_err());
}

// The waiting clock is the only place apply reads envelope.time_ns, so seal this one
// by hand with a distinctive time instead of fold's constant 1.
#[test]
fn decision_sets_then_clears_the_waiting_clock() {
    let qh = [1; 32];
    let mut state = fold(vec![funds(qh)]);
    let pause = ask(qh);
    check_transition(&state, &pause).expect("guard admits pause");
    let env = seal(state.next_event_index, 777, state.last_event_hash, pause);
    apply(&mut state, &env);
    assert_eq!(state.swaps[&qh].status, SwapStatus::WaitingForUser);
    assert_eq!(state.swaps[&qh].waiting_since_ns, Some(777));

    let resume = Event::DecisionMade {
        quote_hash: qh,
        choice: Choice::Requote,
    };
    check_transition(&state, &resume).expect("guard admits resume");
    let env = seal(state.next_event_index, 900, state.last_event_hash, resume);
    apply(&mut state, &env);
    assert_eq!(state.swaps[&qh].status, SwapStatus::Executing);
    assert_eq!(state.swaps[&qh].waiting_since_ns, None);
}
