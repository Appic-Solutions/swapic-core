use crate::state::{MemoryStore, State, Store};
use types::events::{Event, EventType};
use types::TransitionError;

impl<S: Store> State<S> {
    /// Every rule that can refuse an event. The match is exhaustive with no wildcard arm,
    /// so a new variant fails to compile until someone decides its rule. Every checked
    /// computation [`apply_state_transition`] relies on is run here first.
    pub fn check(&self, event: &EventType) -> Result<(), TransitionError> {
        match event {
            EventType::FundsReceived { quote_hash, .. } => match self.store().swap(quote_hash) {
                Some(_) => Err(TransitionError::SwapExists(*quote_hash)),
                None => Ok(()),
            },
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
            // one open question at a time: re-asking would silently re-arm the deadline
            EventType::DecisionRequired { quote_hash, .. } => {
                let swap = self.swap(quote_hash)?;
                swap.ensure_not_closed()?;
                swap.ensure_not_waiting()
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
        // audit lines for deploy-time truth that lives in its own stable cell
        EventType::ConfigChanged { .. } | EventType::RolesChanged { .. } => {}
    }
}

/// Folds a log into a fresh heap state.
pub fn replay(events: impl IntoIterator<Item = Event>) -> State<MemoryStore> {
    let mut state = State::default();
    for event in events {
        apply_state_transition(&mut state, &event);
    }
    state
}

#[cfg(test)]
mod tests;
