use crate::state::AppState;
use types::events::{Choice, Event, EventType};
use types::{Attempt, ChainId, Pocket, QuoteHash, Swap, SwapStatus, TokenAmount};

fn swap<'a>(state: &'a AppState, quote_hash: &QuoteHash) -> Result<&'a Swap, String> {
    state
        .swaps
        .get(quote_hash)
        .ok_or_else(|| "unknown swap".to_string())
}

fn pocket<'a>(state: &'a AppState, chain_id: &ChainId) -> Result<&'a Pocket, String> {
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
/// The match is exhaustive with no wildcard arm, so a new `EventType` variant fails to
/// compile until someone decides what its rule is.
pub fn check_transition(state: &AppState, event: &EventType) -> Result<(), String> {
    match event {
        EventType::FundsReceived { quote_hash, .. } => require(
            !state.swaps.contains_key(quote_hash),
            "swap already has funds",
        ),
        // the sign-before-send law: one open attempt at a time, numbered without gaps
        EventType::TxSigned {
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
            let next = s.last_attempt.map_or(Some(Attempt::FIRST), Attempt::next);
            require(next == Some(*attempt), "attempt out of sequence")
        }
        EventType::TxConfirmed {
            quote_hash,
            attempt,
            ..
        }
        | EventType::TxFailed {
            quote_hash,
            attempt,
            ..
        } => {
            let s = swap(state, quote_hash)?;
            require(s.open_attempt == Some(*attempt), "attempt is not open")
        }
        EventType::PaidInStable { quote_hash, .. } => {
            let s = swap(state, quote_hash)?;
            require(
                matches!(s.status, SwapStatus::Executing | SwapStatus::FundsReceived),
                "swap is not executing",
            )?;
            // fires at most once per swap, so a requote cannot overwrite the recorded amount
            require(
                s.amount_paid == TokenAmount::ZERO,
                "swap is already paid in stable",
            )?;
            require(s.open_attempt.is_none(), "an attempt is still open")
        }
        EventType::DecisionRequired { quote_hash, .. } => {
            let s = swap(state, quote_hash)?;
            require(!s.status.is_closed(), "swap is closed")?;
            // one open question at a time: re-asking would silently re-arm the deadline
            require(
                s.status != SwapStatus::WaitingForUser,
                "swap is already waiting for the user",
            )
        }
        EventType::DecisionMade { quote_hash, .. } => require(
            swap(state, quote_hash)?.status == SwapStatus::WaitingForUser,
            "swap is not waiting for the user",
        ),
        EventType::RefundStarted { quote_hash, .. } => {
            let s = swap(state, quote_hash)?;
            require(
                !s.status.is_closed() && s.status != SwapStatus::Refunding,
                "swap cannot start a refund",
            )
        }
        EventType::Refunded { quote_hash, .. } => {
            let s = swap(state, quote_hash)?;
            require(s.status == SwapStatus::Refunding, "swap is not refunding")?;
            require(s.open_attempt.is_none(), "an attempt is still open")
        }
        EventType::SwapDone { quote_hash } => {
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
        EventType::Frozen { quote_hash, .. } => {
            let s = swap(state, quote_hash)?;
            require(!s.status.is_closed(), "swap is closed")?;
            require(s.open_attempt.is_none(), "an attempt is still open")
        }
        EventType::FeeAccrued { quote_hash, .. } => swap(state, quote_hash).map(|_| ()),
        // the three per-swap pocket moves all name a swap, so all three check it exists
        EventType::PocketReserved {
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
        EventType::PocketReleased {
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
        EventType::PocketSpent {
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
        EventType::PocketRebalanced {
            from_chain, amount, ..
        } => require(
            pocket(state, from_chain)?.available >= *amount,
            "pocket is short",
        ),
        // always legal; variants are named rather than matched by `_` so a new one
        // has to be classified here instead of silently defaulting to legal
        EventType::ConfigChanged { .. }
        | EventType::PocketFunded { .. }
        | EventType::RolesChanged { .. } => Ok(()),
    }
}

fn with_swap(state: &mut AppState, quote_hash: &QuoteHash, f: impl FnOnce(&mut Swap)) {
    if let Some(s) = state.swaps.get_mut(quote_hash) {
        f(s);
    }
}

fn add(a: TokenAmount, b: TokenAmount) -> TokenAmount {
    a.checked_add(b)
        .expect("BUG: a sum of u128 amounts stays far below u256::MAX")
}

fn sub(a: TokenAmount, b: TokenAmount) -> TokenAmount {
    a.checked_sub(b)
        .expect("BUG: check_transition refuses to take more than a pocket holds")
}

/// Pure: no ic-cdk calls. Rejection belongs in `check_transition`, not here.
pub fn apply(state: &mut AppState, event: &Event) {
    state.next_event_index = event
        .index
        .next()
        .expect("BUG: a log cannot hold u64::MAX events");
    state.last_event_hash = event.hash;
    match &event.payload {
        EventType::FundsReceived {
            quote_hash,
            quote_bytes,
            chain_id,
            token,
            amount,
            ..
        } => {
            state.swaps.insert(
                *quote_hash,
                Swap {
                    quote_bytes: quote_bytes.clone(),
                    status: SwapStatus::FundsReceived,
                    last_attempt: None,
                    open_attempt: None,
                    src_chain: *chain_id,
                    src_token: token.clone(),
                    amount_in: *amount,
                    amount_paid: TokenAmount::ZERO,
                    waiting_since: None,
                },
            );
        }
        EventType::TxSigned {
            quote_hash,
            attempt,
            ..
        } => with_swap(state, quote_hash, |s| {
            s.last_attempt = Some(*attempt);
            s.open_attempt = Some(*attempt);
            match s.status {
                SwapStatus::FundsReceived => s.status = SwapStatus::Executing,
                SwapStatus::PaidInStable => s.status = SwapStatus::Delivering,
                _ => {}
            }
        }),
        EventType::TxConfirmed { quote_hash, .. } | EventType::TxFailed { quote_hash, .. } => {
            with_swap(state, quote_hash, |s| s.open_attempt = None)
        }
        EventType::PaidInStable {
            quote_hash, amount, ..
        } => with_swap(state, quote_hash, |s| {
            s.status = SwapStatus::PaidInStable;
            s.amount_paid = *amount;
        }),
        EventType::DecisionRequired { quote_hash, .. } => with_swap(state, quote_hash, |s| {
            s.status = SwapStatus::WaitingForUser;
            s.waiting_since = Some(event.timestamp);
        }),
        EventType::DecisionMade { quote_hash, choice } => with_swap(state, quote_hash, |s| {
            s.waiting_since = None;
            s.status = match choice {
                Choice::Requote => SwapStatus::Executing,
                Choice::Refund => SwapStatus::Refunding,
            };
        }),
        // a refund answers any open question, so the waiting clock stops with it
        EventType::RefundStarted { quote_hash, .. } => with_swap(state, quote_hash, |s| {
            s.status = SwapStatus::Refunding;
            s.waiting_since = None;
        }),
        EventType::Refunded { quote_hash, .. } => {
            with_swap(state, quote_hash, |s| s.status = SwapStatus::Refunded)
        }
        EventType::SwapDone { quote_hash } => {
            with_swap(state, quote_hash, |s| s.status = SwapStatus::Done)
        }
        EventType::Frozen { quote_hash, .. } => with_swap(state, quote_hash, |s| {
            s.status = SwapStatus::Frozen;
            s.waiting_since = None;
        }),
        EventType::FeeAccrued { amount, .. } => {
            state.fees_accrued = add(state.fees_accrued, *amount);
        }
        EventType::PocketFunded { chain_id, amount } => {
            let p = state.pockets.entry(*chain_id).or_default();
            p.available = add(p.available, *amount);
        }
        EventType::PocketReserved {
            chain_id, amount, ..
        } => {
            let p = state.pockets.entry(*chain_id).or_default();
            p.available = sub(p.available, *amount);
            p.reserved = add(p.reserved, *amount);
        }
        EventType::PocketReleased {
            chain_id, amount, ..
        } => {
            let p = state.pockets.entry(*chain_id).or_default();
            p.reserved = sub(p.reserved, *amount);
            p.available = add(p.available, *amount);
        }
        // the settle debit: the value left the pocket on-chain, so it does not come back
        EventType::PocketSpent {
            chain_id, amount, ..
        } => {
            let p = state.pockets.entry(*chain_id).or_default();
            p.reserved = sub(p.reserved, *amount);
        }
        EventType::PocketRebalanced {
            from_chain,
            to_chain,
            amount,
            ..
        } => {
            let from = state.pockets.entry(*from_chain).or_default();
            from.available = sub(from.available, *amount);
            let to = state.pockets.entry(*to_chain).or_default();
            to.available = add(to.available, *amount);
        }
        // audit lines: they record a change to deploy-time truth that lives in its own
        // stable cell, so the folded swap state is untouched
        EventType::ConfigChanged { .. } | EventType::RolesChanged { .. } => {}
    }
}

pub fn replay(events: impl Iterator<Item = Event>) -> AppState {
    let mut state = AppState::default();
    for event in events {
        apply(&mut state, &event);
    }
    state
}

#[cfg(test)]
mod tests;
