use crate::state::{MemoryStore, State, Store};
use thiserror::Error;
use types::checked_amount::CheckedAmountOf;
use types::events::{Event, EventType, TxPurpose};
use types::quote::quote_hash_of;
use types::{ChainId, EventHash, EventIndex, Nonce, NonceKey, Quote, QuoteHash, TransitionError};

/// Every field a canonical preimage writes in sixteen bytes must fit in them, whatever
/// unit it counts. Checked here so an event with no preimage is refused by the guard
/// rather than failing at the seal.
fn in_preimage_range<Unit>(amount: CheckedAmountOf<Unit>) -> Result<(), TransitionError> {
    if amount.try_into_u128().is_none() {
        return Err(TransitionError::AmountOutOfRange(amount.change_units()));
    }
    Ok(())
}

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
            // a paused swap moves nothing until the user answers, and the record spends a
            // number the swap is still holding
            EventType::TxSigned {
                quote_hash,
                attempt,
                chain_id,
                ..
            } => {
                let swap = self.swap(quote_hash)?;
                swap.ensure_not_closed()?;
                swap.ensure_not_waiting()?;
                swap.ensure_no_open_attempt()?;
                swap.ensure_next_attempt(*attempt)?;
                self.ensure_holds_unsigned_nonce(quote_hash, *chain_id)
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
            // rule A4, the one line that makes a double spend of a nonce impossible: the
            // number the allocator is at is the only number a transaction may carry, so
            // two transactions cannot share one however their calls interleave. The swap
            // rules are `TxSigned`'s in full, checked here as well because a transaction
            // created for a swap that can never sign it would strand the nonce it
            // allocated, and one swap never holds two numbers at once.
            EventType::TxCreated {
                purpose,
                chain_id,
                nonce,
                value,
                gas_limit,
                max_fee,
                max_priority_fee,
                ..
            } => {
                in_preimage_range(*value)?;
                in_preimage_range(*gas_limit)?;
                in_preimage_range(*max_fee)?;
                in_preimage_range(*max_priority_fee)?;
                self.ensure_can_send(purpose)?;
                self.ensure_next_nonce(*chain_id, *nonce)
            }
            // a replacement keeps the nonce of what it replaces, so it allocates nothing
            // and must name a nonce the allocator already handed out (rule A5). One that
            // is a swap's attempt must be that swap's open attempt; a pull and a cancel
            // have no swap to ask.
            EventType::TxReplaced {
                purpose,
                chain_id,
                nonce,
                max_fee,
                max_priority_fee,
                ..
            } => {
                in_preimage_range(*max_fee)?;
                in_preimage_range(*max_priority_fee)?;
                if let Some(quote_hash) = purpose.attempt_of() {
                    let swap = self.swap(&quote_hash)?;
                    if swap.open_attempt.is_none() {
                        return Err(TransitionError::NoOpenAttempt(quote_hash));
                    }
                }
                self.ensure_allocated_nonce(*chain_id, *nonce)
            }
            // the other end of rule A5: a cancel spends a number that was handed out and
            // never signed for, and nothing else. A nonce with a signed transaction behind
            // it is not one to cancel, and a number that was never handed out is not one to
            // spend, so both are refused here rather than put on a chain.
            EventType::TxCancelled {
                chain_id, nonce, ..
            } => self.ensure_unsigned_nonce(*chain_id, *nonce),
            // the pull's signed record: `TxSigned` for a transaction with no swap behind it.
            // It spends the number handed out for this pull and nothing else, so a late
            // record of a cancelled pull, a swap's number, and another quote's pull are all
            // refused, as they are for a swap.
            EventType::PullSigned {
                quote_hash,
                chain_id,
                nonce,
                ..
            } => self.ensure_pull_nonce(*quote_hash, *chain_id, *nonce),
            // the repair of a divergence: it makes a swap's index entries agree with the
            // swap, dropping what the swap does not imply and keeping what it does. Admitted
            // on nothing, because nothing here could tell: the entry it drops is in no log,
            // so a rule that read the index would refuse on replay the very line that
            // explains the repair, and the swap alone cannot tell a stale entry from the
            // wait the swap is really in. Where the index already agrees the repair changes
            // nothing, so it can never strand a wait.
            EventType::WaitingRepaired { .. } => Ok(()),
            // always legal; named rather than matched by `_` so a new variant has to be
            // classified here instead of silently defaulting to legal
            EventType::ConfigChanged { .. } | EventType::RolesChanged { .. } => Ok(()),
        }
    }

    /// Whether a transaction may be created for `purpose`: a swap it names has to exist and
    /// be one an attempt can still be signed for, and it must not already be holding a
    /// number it has not signed for. These are `TxSigned`'s rules, minus the attempt number
    /// a `TxCreated` does not carry, so a creation this guard admits is one the signed
    /// record will admit too. A pull is for a quote nobody has paid yet, so it needs the
    /// swap NOT to exist and is held to one number at a time like a swap. A cancel names no
    /// swap.
    fn ensure_can_send(&self, purpose: &TxPurpose) -> Result<(), TransitionError> {
        let Some(quote_hash) = purpose.quote_hash() else {
            return Ok(());
        };
        match purpose {
            TxPurpose::GaslessPull(_) => {
                if self.store().swap(&quote_hash).is_some() {
                    return Err(TransitionError::SwapExists(quote_hash));
                }
            }
            _ => {
                let swap = self.swap(&quote_hash)?;
                swap.ensure_not_closed()?;
                swap.ensure_not_waiting()?;
                swap.ensure_no_open_attempt()?;
            }
        }
        // rule A4 from the swap's side: two sends for ONE swap that interleave at the
        // signature would otherwise both allocate, and the second's `TxSigned` would be
        // refused with its number already spent
        if self.store().has_unsigned_nonce_for(&quote_hash) {
            return Err(TransitionError::NonceStillUnsigned(quote_hash));
        }
        Ok(())
    }

    /// Rule A4: the nonce a transaction carries is the one the allocator is at.
    fn ensure_next_nonce(&self, chain_id: ChainId, nonce: Nonce) -> Result<(), TransitionError> {
        let expected = self.next_nonce(&chain_id);
        if nonce != expected {
            return Err(TransitionError::NonceOutOfSequence {
                chain_id,
                nonce,
                expected,
            });
        }
        // the allocator has to have a successor to move to, which is a different refusal:
        // reporting it as a number that is not the one expected would read as a
        // contradiction, because it is exactly the number expected
        if nonce.next().is_none() {
            return Err(TransitionError::NonceExhausted { chain_id });
        }
        Ok(())
    }

    /// A nonce the allocator has already handed out on `chain_id`.
    fn ensure_allocated_nonce(
        &self,
        chain_id: ChainId,
        nonce: Nonce,
    ) -> Result<(), TransitionError> {
        let next = self.next_nonce(&chain_id);
        if nonce >= next {
            return Err(TransitionError::NonceNeverAllocated {
                chain_id,
                nonce,
                next,
            });
        }
        Ok(())
    }

    /// The mirror of [`Self::ensure_unsigned_nonce`] from the swap's side: a signed record
    /// spends the number the swap is holding, so the swap has to be holding one, on the
    /// chain the record names. The signature is awaited across consensus rounds on the
    /// signing subnet, and a cancel that spent the number meanwhile has sealed it (rule
    /// A5); the record that comes back late is refused here, which is what keeps two signed
    /// transactions off one nonce.
    fn ensure_holds_unsigned_nonce(
        &self,
        quote_hash: &QuoteHash,
        chain_id: ChainId,
    ) -> Result<(), TransitionError> {
        match self.store().unsigned_nonce_of(quote_hash) {
            Some(key) if key.chain_id == chain_id => Ok(()),
            _ => Err(TransitionError::NoUnsignedNonce {
                quote_hash: *quote_hash,
                chain_id,
            }),
        }
    }

    /// The mirror of [`Self::ensure_holds_unsigned_nonce`] for a pull, which has no swap to
    /// find its number by: the record names the number, and that number must be waiting for
    /// exactly this quote's pull.
    fn ensure_pull_nonce(
        &self,
        quote_hash: QuoteHash,
        chain_id: ChainId,
        nonce: Nonce,
    ) -> Result<(), TransitionError> {
        let held = self
            .store()
            .unsigned_nonce(&NonceKey { chain_id, nonce })
            .map(|unsigned| unsigned.purpose);
        if held != Some(TxPurpose::GaslessPull(quote_hash)) {
            return Err(TransitionError::NonceNotHeldForPull {
                chain_id,
                nonce,
                quote_hash,
            });
        }
        Ok(())
    }

    /// A nonce that was handed out and is still waiting for a signed record.
    fn ensure_unsigned_nonce(
        &self,
        chain_id: ChainId,
        nonce: Nonce,
    ) -> Result<(), TransitionError> {
        if self
            .store()
            .unsigned_nonce(&NonceKey { chain_id, nonce })
            .is_none()
        {
            return Err(TransitionError::NonceNotUnsigned { chain_id, nonce });
        }
        Ok(())
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
        EventType::TxCreated {
            purpose,
            chain_id,
            nonce,
            ..
        } => state.record_nonce_allocated(*chain_id, *nonce, *purpose, event.timestamp),
        EventType::TxCancelled {
            chain_id, nonce, ..
        }
        | EventType::PullSigned {
            chain_id, nonce, ..
        } => state.record_nonce_spent(*chain_id, *nonce),
        // audit lines for deploy-time truth that lives in its own stable cell, and for a
        // re-send that allocates nothing and closes nothing
        EventType::ConfigChanged { .. }
        | EventType::RolesChanged { .. }
        | EventType::TxReplaced { .. } => {}
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
