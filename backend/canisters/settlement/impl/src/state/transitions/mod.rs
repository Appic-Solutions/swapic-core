use crate::state::{AppState, Pocket};
use settlement_api::types::events::{Choice, Event, EventEnvelope, Hash32};
use settlement_api::types::swap::{SwapState, SwapStatus};

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
        Event::DecisionRequired { quote_hash, .. } => {
            let s = swap(state, quote_hash)?;
            require(!s.status.is_closed(), "swap is closed")?;
            // one open question at a time: re-asking would silently re-arm the deadline
            require(
                s.status != SwapStatus::WaitingForUser,
                "swap is already waiting for the user",
            )
        }
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
        // the three per-swap pocket moves all name a swap, so all three check it exists
        Event::PocketReserved {
            quote_hash,
            chain_id,
            amount,
        } => {
            swap(state, quote_hash)?;
            require(
                pocket(state, chain_id)?.available >= *amount,
                "pocket is short",
            )
        }
        Event::PocketReleased {
            quote_hash,
            chain_id,
            amount,
        } => {
            swap(state, quote_hash)?;
            require(
                pocket(state, chain_id)?.reserved >= *amount,
                "pocket reservation is short",
            )
        }
        Event::PocketSpent {
            quote_hash,
            chain_id,
            amount,
        } => {
            swap(state, quote_hash)?;
            require(
                pocket(state, chain_id)?.reserved >= *amount,
                "pocket reservation is short",
            )
        }
        Event::PocketRebalanced {
            from_chain, amount, ..
        } => require(
            pocket(state, from_chain)?.available >= *amount,
            "pocket is short",
        ),
        // always legal; variants are named rather than matched by `_` so a new one
        // has to be classified here instead of silently defaulting to legal
        Event::ConfigChanged { .. } | Event::PocketFunded { .. } | Event::RolesChanged { .. } => {
            Ok(())
        }
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
        // a refund answers any open question, so the waiting clock stops with it
        Event::RefundStarted { quote_hash, .. } => with_swap(state, quote_hash, |s| {
            s.status = SwapStatus::Refunding;
            s.waiting_since_ns = None;
        }),
        Event::Refunded { quote_hash, .. } => {
            with_swap(state, quote_hash, |s| s.status = SwapStatus::Refunded)
        }
        Event::SwapDone { quote_hash } => {
            with_swap(state, quote_hash, |s| s.status = SwapStatus::Done)
        }
        Event::Frozen { quote_hash, .. } => with_swap(state, quote_hash, |s| {
            s.status = SwapStatus::Frozen;
            s.waiting_since_ns = None;
        }),
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
        // the settle debit: the value left the pocket on-chain, so it does not come back
        Event::PocketSpent {
            chain_id, amount, ..
        } => {
            let p = state.pockets.entry(*chain_id).or_default();
            p.reserved = p.reserved.saturating_sub(*amount);
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
        // audit lines: they record a change to deploy-time truth that lives in its own
        // stable cell, so the folded swap state is untouched
        Event::ConfigChanged { .. } | Event::RolesChanged { .. } => {}
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
mod tests;
