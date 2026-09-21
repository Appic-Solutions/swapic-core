use crate::state::{MemoryStore, State, Store};
use thiserror::Error;
use types::events::{Event, EventType};
use types::quote::quote_hash_of;
use types::{EventHash, EventIndex, Quote, TransitionError};

impl<S: Store> State<S> {
    /// Every rule that can refuse an event. The match is exhaustive with no wildcard arm,
    /// so a new variant fails to compile until someone decides its rule. Every checked
    /// computation [`apply_state_transition`] relies on is run here first.
    pub fn check(&self, event: &EventType) -> Result<(), TransitionError> {
        // before any rule: an amount no 16-byte canonical field holds could never be sealed
        if let Some(amount) = event
            .amount()
            .filter(|amount| amount.try_into_u128().is_none())
        {
            return Err(TransitionError::AmountOutOfRange(amount));
        }
        match event {
            // the swap id must be the hash of the preimage recorded with it: the sweep reads
            // `auto_refund` out of those bytes to decide refund policy, so a pair that does
            // not bind would refund a user who asked to be consulted, or leave a swap
            // waiting forever. The preimage must also be a quote this canister would hold:
            // parsing is the layout and `validate` is the rest, so the log cannot record a
            // version this wasm does not read, or the empty fields `register_quote` refuses
            EventType::FundsReceived {
                quote_hash,
                quote_bytes,
                ..
            } => {
                if self.store().swap(quote_hash).is_some() {
                    return Err(TransitionError::SwapExists(*quote_hash));
                }
                Quote::parse(quote_bytes)?.validate()?;
                let computed = quote_hash_of(quote_bytes);
                if computed != *quote_hash {
                    return Err(TransitionError::QuoteHashMismatch {
                        declared: *quote_hash,
                        computed,
                    });
                }
                Ok(())
            }
            // the sign-before-send law: one open attempt at a time, numbered without gaps,
            // and a paused swap moves nothing until the user answers
            EventType::TxSigned {
                quote_hash,
                attempt,
                ..
            } => {
                let swap = self.swap(quote_hash)?;
                swap.ensure_not_closed()?;
                swap.ensure_not_waiting()?;
                swap.ensure_no_open_attempt()?;
                swap.ensure_next_attempt(*attempt)
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
            } => self.swap(quote_hash)?.ensure_attempt_open(*attempt),
            EventType::PaidInStable { quote_hash, .. } => {
                let swap = self.swap(quote_hash)?;
                swap.ensure_executing()?;
                swap.ensure_unpaid()?;
                swap.ensure_no_open_attempt()
            }
            // one open question at a time: re-asking would silently re-arm the deadline.
            // A refund is one way, and a question with an attempt in flight would be
            // answered while that attempt is still deciding itself on the chain.
            EventType::DecisionRequired { quote_hash, .. } => {
                let swap = self.swap(quote_hash)?;
                swap.ensure_not_closed()?;
                swap.ensure_not_refunding()?;
                swap.ensure_not_waiting()?;
                swap.ensure_no_open_attempt()
            }
            EventType::DecisionMade { quote_hash, .. } => self.swap(quote_hash)?.ensure_waiting(),
            EventType::RefundStarted { quote_hash, .. } => {
                self.swap(quote_hash)?.ensure_can_start_refund()
            }
            EventType::Refunded { quote_hash, .. } => {
                let swap = self.swap(quote_hash)?;
                swap.ensure_refunding()?;
                swap.ensure_no_open_attempt()
            }
            EventType::SwapDone { quote_hash } => {
                let swap = self.swap(quote_hash)?;
                swap.ensure_in_flight()?;
                swap.ensure_no_open_attempt()
            }
            EventType::Frozen { quote_hash, .. } => {
                let swap = self.swap(quote_hash)?;
                swap.ensure_not_closed()?;
                swap.ensure_no_open_attempt()
            }
            EventType::FeeAccrued { quote_hash, amount } => {
                self.swap(quote_hash)?;
                self.meta()
                    .fees_accrued
                    .checked_add(*amount)
                    .ok_or(TransitionError::FeesOverflow)
                    .map(drop)
            }
            EventType::PocketFunded { chain_id, amount } => {
                self.pocket_or_empty(chain_id).fund(*amount)?;
                Ok(())
            }
            // the three per-swap pocket moves all name a swap, so all three check it exists
            EventType::PocketReserved {
                quote_hash,
                chain_id,
                amount,
            } => {
                self.swap(quote_hash)?;
                self.pocket(chain_id)?.reserve(*amount)?;
                Ok(())
            }
            EventType::PocketReleased {
                quote_hash,
                chain_id,
                amount,
            } => {
                self.swap(quote_hash)?;
                self.pocket(chain_id)?.release(*amount)?;
                Ok(())
            }
            EventType::PocketSpent {
                quote_hash,
                chain_id,
                amount,
            } => {
                self.swap(quote_hash)?;
                self.pocket(chain_id)?.spend(*amount)?;
                Ok(())
            }
            EventType::PocketRebalanced {
                from_chain,
                to_chain,
                amount,
                ..
            } => {
                let source = self.pocket(from_chain)?.withdraw(*amount)?;
                let destination = if to_chain == from_chain {
                    source
                } else {
                    self.pocket_or_empty(to_chain)
                };
                destination.fund(*amount)?;
                Ok(())
            }
            // the repair of a divergence, so it is admitted on what it repairs and not on
            // the index: a swap that is not waiting for its user, or no swap at all. The
            // entry it drops is in no log, so a replay never holds it, and a rule that read
            // the index would refuse on replay the very line that explains the repair. An
            // entry whose swap really is waiting is the index doing its job, and dropping it
            // would strand the wait.
            EventType::WaitingRepaired { quote_hash } => match self.store().swap(quote_hash) {
                Some(swap) => swap.ensure_not_waiting(),
                None => Ok(()),
            },
            // always legal; named rather than matched by `_` so a new variant has to be
            // classified here instead of silently defaulting to legal
            EventType::ConfigChanged { .. } | EventType::RolesChanged { .. } => Ok(()),
        }
    }
}

/// Records an event [`State::check`] admitted. Pure: no canister calls, and nothing here
/// refuses.
pub fn apply_state_transition<S: Store>(state: &mut State<S>, event: &Event) {
    state.record_event(event.index, event.hash);
    match &event.payload {
        EventType::FundsReceived {
            quote_hash,
            quote_bytes,
            chain_id,
            token,
            amount,
            ..
        } => state.record_funds_received(
            *quote_hash,
            quote_bytes.clone(),
            *chain_id,
            token.clone(),
            *amount,
        ),
        EventType::TxSigned {
            quote_hash,
            attempt,
            ..
        } => state.record_attempt_signed(quote_hash, *attempt),
        EventType::TxConfirmed { quote_hash, .. } | EventType::TxFailed { quote_hash, .. } => {
            state.record_attempt_closed(quote_hash)
        }
        EventType::PaidInStable {
            quote_hash, amount, ..
        } => state.record_paid_in_stable(quote_hash, *amount),
        EventType::DecisionRequired { quote_hash, .. } => {
            state.record_decision_required(quote_hash, event.timestamp)
        }
        EventType::DecisionMade { quote_hash, choice } => {
            state.record_decision_made(quote_hash, *choice)
        }
        EventType::RefundStarted { quote_hash, .. } => state.record_refund_started(quote_hash),
        EventType::Refunded { quote_hash, .. } => state.record_refunded(quote_hash),
        EventType::SwapDone { quote_hash } => state.record_swap_done(quote_hash),
        EventType::Frozen { quote_hash, .. } => state.record_frozen(quote_hash),
        EventType::FeeAccrued { amount, .. } => state.record_fee_accrued(*amount),
        EventType::PocketFunded { chain_id, amount } => {
            state.record_pocket_funded(*chain_id, *amount)
        }
        EventType::PocketReserved {
            chain_id, amount, ..
        } => state.record_pocket_reserved(*chain_id, *amount),
        EventType::PocketReleased {
            chain_id, amount, ..
        } => state.record_pocket_released(*chain_id, *amount),
        EventType::PocketSpent {
            chain_id, amount, ..
        } => state.record_pocket_spent(*chain_id, *amount),
        EventType::PocketRebalanced {
            from_chain,
            to_chain,
            amount,
            ..
        } => state.record_pocket_rebalanced(*from_chain, *to_chain, *amount),
        EventType::WaitingRepaired { quote_hash } => state.record_waiting_repaired(quote_hash),
        // audit lines for deploy-time truth that lives in its own stable cell
        EventType::ConfigChanged { .. } | EventType::RolesChanged { .. } => {}
    }
}

/// Why a log does not fold. The replay stopped at `index` and applied nothing from there.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ReplayError {
    #[error("event {found} sits where event {expected} belongs")]
    OutOfSequence {
        expected: EventIndex,
        found: EventIndex,
    },
    #[error("event {index} links to {parent} but the fold ends with {head}")]
    Unlinked {
        index: EventIndex,
        parent: EventHash,
        head: EventHash,
    },
    #[error("event {index} has no successor index")]
    LogFull { index: EventIndex },
    #[error("event {index} is refused: {error}")]
    Refused {
        index: EventIndex,
        error: TransitionError,
    },
}

/// Folds a log into a fresh heap state, admitting each event the way `append_event` did:
/// in sequence, linked to the head, and through [`State::check`]. A log a bug or a later
/// wasm made inconsistent is an `Err`, never a panic, so the audit can halt on it.
pub fn replay(events: impl IntoIterator<Item = Event>) -> Result<State<MemoryStore>, ReplayError> {
    let mut state = State::default();
    replay_into(&mut state, events)?;
    Ok(state)
}

/// [`replay`] continued: folds `events` onto a heap state that already holds the fold of
/// the events before them, so a log too long to fold in one message is folded across many.
/// The first event must be the one the state seals next. On an `Err` the state holds the
/// events before the one refused, and nothing of it.
pub fn replay_into(
    state: &mut State<MemoryStore>,
    events: impl IntoIterator<Item = Event>,
) -> Result<(), ReplayError> {
    for event in events {
        let meta = state.meta();
        let index = event.index;
        if index != meta.next_event_index {
            return Err(ReplayError::OutOfSequence {
                expected: meta.next_event_index,
                found: index,
            });
        }
        if event.parent_hash != meta.last_event_hash {
            return Err(ReplayError::Unlinked {
                index,
                parent: event.parent_hash,
                head: meta.last_event_hash,
            });
        }
        if index.next().is_none() {
            return Err(ReplayError::LogFull { index });
        }
        state
            .check(&event.payload)
            .map_err(|error| ReplayError::Refused { index, error })?;
        apply_state_transition(state, &event);
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests;
