use crate::events::{Choice, Event, EventEnvelope, Hash32};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SwapStatus {
    FundsReceived,
    Executing,
    PaidInStable,
    Delivering,
    WaitingForUser,
    Done,
    Refunding,
    Refunded,
    Frozen,
}

impl SwapStatus {
    /// Terminal: no further work is ever scheduled for the swap.
    fn is_closed(self) -> bool {
        matches!(self, Self::Done | Self::Refunded | Self::Frozen)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SwapState {
    pub quote_bytes: Vec<u8>,
    pub status: SwapStatus,
    pub attempts: u32,
    pub open_attempt: Option<u32>,
    pub src_chain: u64,
    pub src_token: String,
    pub amount_in: u128,
    pub amount_paid: u128,
    pub waiting_since_ns: Option<u64>,
}

#[derive(Default, Clone, Debug, PartialEq)]
pub struct Pocket {
    pub available: u128,
    pub reserved: u128,
}

#[derive(Default, Clone, Debug, PartialEq)]
pub struct AppState {
    pub swaps: BTreeMap<Hash32, SwapState>,
    pub pockets: BTreeMap<u64, Pocket>,
    pub fees_accrued: u128,
    pub next_event_index: u64,
    pub last_event_hash: Hash32,
}

fn swap<'a>(state: &'a AppState, quote_hash: &Hash32) -> Result<&'a SwapState, String> {
    state
        .swaps
        .get(quote_hash)
        .ok_or_else(|| "unknown swap".to_string())
}

fn pocket<'a>(state: &'a AppState, chain_id: &u64) -> Result<&'a Pocket, String> {
    state
        .pockets
        .get(chain_id)
        .ok_or_else(|| format!("no pocket on chain {chain_id}"))
}

fn require(ok: bool, msg: &str) -> Result<(), String> {
    if ok {
        Ok(())
    } else {
        Err(msg.to_string())
    }
}

/// Every rule that can reject an event lives here; `apply` assumes it already passed.
/// The match is exhaustive with no wildcard arm, so a new `Event` variant fails to
/// compile until someone decides what its rule is.
pub fn check_transition(state: &AppState, event: &Event) -> Result<(), String> {
    match event {
        Event::FundsReceived { quote_hash, .. } => require(
            !state.swaps.contains_key(quote_hash),
            "swap already has funds",
        ),
        // the sign-before-send law: one open attempt at a time, numbered without gaps
        Event::TxSigned {
            quote_hash,
            attempt,
            ..
        } => {
            let s = swap(state, quote_hash)?;
            require(!s.status.is_closed(), "swap is closed")?;
            // a paused swap moves nothing until the user answers
            require(
                s.status != SwapStatus::WaitingForUser,
                "swap is waiting for the user",
            )?;
            require(s.open_attempt.is_none(), "an attempt is still open")?;
            require(
                *attempt == s.attempts.saturating_add(1),
                "attempt out of sequence",
            )
        }
        Event::TxConfirmed {
            quote_hash,
            attempt,
            ..
        }
        | Event::TxFailed {
            quote_hash,
            attempt,
            ..
        } => {
            let s = swap(state, quote_hash)?;
            require(s.open_attempt == Some(*attempt), "attempt is not open")
        }
        Event::PaidInStable { quote_hash, .. } => {
            let s = swap(state, quote_hash)?;
            require(
                matches!(s.status, SwapStatus::Executing | SwapStatus::FundsReceived),
                "swap is not executing",
            )?;
            // fires at most once per swap, so a requote cannot overwrite the recorded amount
            require(s.amount_paid == 0, "swap is already paid in stable")?;
            require(s.open_attempt.is_none(), "an attempt is still open")
        }
        Event::DecisionRequired { quote_hash, .. } => require(
            !swap(state, quote_hash)?.status.is_closed(),
            "swap is closed",
        ),
        Event::DecisionMade { quote_hash, .. } => require(
            swap(state, quote_hash)?.status == SwapStatus::WaitingForUser,
            "swap is not waiting for the user",
        ),
        Event::RefundStarted { quote_hash, .. } => {
            let s = swap(state, quote_hash)?;
            require(
                !s.status.is_closed() && s.status != SwapStatus::Refunding,
                "swap cannot start a refund",
            )
        }
        Event::Refunded { quote_hash, .. } => {
            let s = swap(state, quote_hash)?;
            require(s.status == SwapStatus::Refunding, "swap is not refunding")?;
            require(s.open_attempt.is_none(), "an attempt is still open")
        }
        Event::SwapDone { quote_hash } => {
            let s = swap(state, quote_hash)?;
            require(
                matches!(
                    s.status,
                    SwapStatus::Delivering | SwapStatus::Executing | SwapStatus::PaidInStable
                ),
                "swap is not in flight",
            )?;
            require(s.open_attempt.is_none(), "an attempt is still open")
        }
        Event::Frozen { quote_hash, .. } => {
            let s = swap(state, quote_hash)?;
            require(!s.status.is_closed(), "swap is closed")?;
            require(s.open_attempt.is_none(), "an attempt is still open")
        }
        Event::FeeAccrued { quote_hash, .. } => swap(state, quote_hash).map(|_| ()),
        Event::PocketReserved {
            chain_id, amount, ..
        } => require(
            pocket(state, chain_id)?.available >= *amount,
            "pocket is short",
        ),
        Event::PocketReleased {
            chain_id, amount, ..
        } => require(
            pocket(state, chain_id)?.reserved >= *amount,
            "pocket reservation is short",
        ),
        Event::PocketRebalanced {
            from_chain, amount, ..
        } => require(
            pocket(state, from_chain)?.available >= *amount,
            "pocket is short",
        ),
        // always legal; variants are named rather than matched by `_` so a new one
        // has to be classified here instead of silently defaulting to legal
        Event::ConfigChanged { .. } | Event::PocketFunded { .. } => Ok(()),
    }
}

fn with_swap(state: &mut AppState, quote_hash: &Hash32, f: impl FnOnce(&mut SwapState)) {
    if let Some(s) = state.swaps.get_mut(quote_hash) {
        f(s);
    }
}

/// Pure and total: no ic-cdk calls, and every arithmetic op saturates, so no event the
/// guard admitted can panic here. Rejection belongs in `check_transition`, not here.
pub fn apply(state: &mut AppState, envelope: &EventEnvelope) {
    state.next_event_index = envelope.index.saturating_add(1);
    state.last_event_hash = envelope.hash;
    match &envelope.event {
        Event::FundsReceived {
            quote_hash,
            quote_bytes,
            chain_id,
            token,
            amount,
            ..
        } => {
            state.swaps.insert(
                *quote_hash,
                SwapState {
                    quote_bytes: quote_bytes.clone(),
                    status: SwapStatus::FundsReceived,
                    attempts: 0,
                    open_attempt: None,
                    src_chain: *chain_id,
                    src_token: token.clone(),
                    amount_in: *amount,
                    amount_paid: 0,
                    waiting_since_ns: None,
                },
            );
        }
        Event::TxSigned {
            quote_hash,
            attempt,
            ..
        } => with_swap(state, quote_hash, |s| {
            s.attempts = *attempt;
            s.open_attempt = Some(*attempt);
            match s.status {
                SwapStatus::FundsReceived => s.status = SwapStatus::Executing,
                SwapStatus::PaidInStable => s.status = SwapStatus::Delivering,
                _ => {}
            }
        }),
        Event::TxConfirmed { quote_hash, .. } | Event::TxFailed { quote_hash, .. } => {
            with_swap(state, quote_hash, |s| s.open_attempt = None)
        }
        Event::PaidInStable {
            quote_hash, amount, ..
        } => with_swap(state, quote_hash, |s| {
            s.status = SwapStatus::PaidInStable;
            s.amount_paid = *amount;
        }),
        Event::DecisionRequired { quote_hash, .. } => with_swap(state, quote_hash, |s| {
            s.status = SwapStatus::WaitingForUser;
            s.waiting_since_ns = Some(envelope.time_ns);
        }),
        Event::DecisionMade { quote_hash, choice } => with_swap(state, quote_hash, |s| {
            s.waiting_since_ns = None;
            s.status = match choice {
                Choice::Requote => SwapStatus::Executing,
                Choice::Refund => SwapStatus::Refunding,
            };
        }),
        Event::RefundStarted { quote_hash, .. } => {
            with_swap(state, quote_hash, |s| s.status = SwapStatus::Refunding)
        }
        Event::Refunded { quote_hash, .. } => {
            with_swap(state, quote_hash, |s| s.status = SwapStatus::Refunded)
        }
        Event::SwapDone { quote_hash } => {
            with_swap(state, quote_hash, |s| s.status = SwapStatus::Done)
        }
        Event::Frozen { quote_hash, .. } => {
            with_swap(state, quote_hash, |s| s.status = SwapStatus::Frozen)
        }
        Event::FeeAccrued { amount, .. } => {
            state.fees_accrued = state.fees_accrued.saturating_add(*amount);
        }
        Event::PocketFunded { chain_id, amount } => {
            let p = state.pockets.entry(*chain_id).or_default();
            p.available = p.available.saturating_add(*amount);
        }
        Event::PocketReserved {
            chain_id, amount, ..
        } => {
            let p = state.pockets.entry(*chain_id).or_default();
            p.available = p.available.saturating_sub(*amount);
            p.reserved = p.reserved.saturating_add(*amount);
        }
        Event::PocketReleased {
            chain_id, amount, ..
        } => {
            let p = state.pockets.entry(*chain_id).or_default();
            p.reserved = p.reserved.saturating_sub(*amount);
            p.available = p.available.saturating_add(*amount);
        }
        Event::PocketRebalanced {
            from_chain,
            to_chain,
            amount,
            ..
        } => {
            let from = state.pockets.entry(*from_chain).or_default();
            from.available = from.available.saturating_sub(*amount);
            let to = state.pockets.entry(*to_chain).or_default();
            to.available = to.available.saturating_add(*amount);
        }
        Event::ConfigChanged { .. } => {}
    }
}

pub fn replay(events: impl Iterator<Item = EventEnvelope>) -> AppState {
    let mut state = AppState::default();
    for envelope in events {
        apply(&mut state, &envelope);
    }
    state
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::seal;

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
            Event::DecisionRequired {
                quote_hash: qh,
                reason: "slippage".into(),
            },
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
        let mut state = fold(vec![
            funds(qh),
            signed(qh, 1),
            confirmed(qh, 1),
            Event::DecisionRequired {
                quote_hash: qh,
                reason: "slippage".into(),
            },
        ]);
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
        let pause = Event::DecisionRequired {
            quote_hash: qh,
            reason: "slippage".into(),
        };
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
}
