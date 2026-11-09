use crate::state::{State, Store};
use types::events::{Event, EventType, TxPurpose};
use types::FlowStep;
use types::{ChainId, Nonce, NonceKey, Timestamp, TransitionError};

impl<S: Store> State<S> {
    /// [`State::check`], and the rules that read the line's own time `at` (a pool step's
    /// dedup instant): what an append runs.
    pub fn check_at(&self, event: &EventType, at: Timestamp) -> Result<(), TransitionError> {
        self.check(event)?;
        if let EventType::Pool { step } = event {
            self.pool().admits_at(step, &self.pool_view(), at)?;
        }
        Ok(())
    }

    /// Every rule that can refuse an event. The match is exhaustive with no wildcard arm,
    /// so a new variant fails to compile until someone decides its rule. Every checked
    /// computation [`apply_state_transition`] relies on is run here first.
    pub fn check(&self, event: &EventType) -> Result<(), TransitionError> {
        match event {
            // rule A4, the one line that makes a double spend of a nonce impossible: the
            // number the allocator is at is the only number a transaction may carry, so
            // two transactions cannot share one however their calls interleave. The rules
            // its signed record will be held to are checked here as well, because a
            // transaction created for a record that can never be written would strand the
            // nonce it allocated, and one key never holds two numbers at once.
            EventType::TxCreated {
                purpose,
                chain_id,
                nonce,
                ..
            } => {
                self.ensure_can_send(purpose)?;
                self.ensure_next_nonce(*chain_id, *nonce)
            }
            // a replacement keeps the nonce of what it replaces, so it allocates nothing
            // and must name a nonce the allocator already handed out (rule A5)
            EventType::TxReplaced {
                chain_id, nonce, ..
            } => self.ensure_allocated_nonce(*chain_id, *nonce),
            // the other end of rule A5: a cancel spends a number that was handed out and
            // never signed for, and nothing else. A nonce with a signed transaction behind
            // it is not one to cancel, and a number that was never handed out is not one to
            // spend, so both are refused here rather than put on a chain.
            EventType::TxCancelled {
                chain_id, nonce, ..
            } => self.ensure_unsigned_nonce(*chain_id, *nonce),
            EventType::Books { chain_id, step } => Ok(self.book(chain_id).admits(step)?),
            // a pool step that records a signed transaction spends the nonce handed out for
            // exactly its purpose, as a flow's does
            EventType::Pool { step } => {
                self.pool().admits(step, &self.pool_view())?;
                match (step.signed_tx(), step.signed_purpose()) {
                    (Some((chain_id, nonce, _, _)), Some(purpose)) => {
                        self.ensure_flow_nonce(purpose, chain_id, nonce)
                    }
                    _ => Ok(()),
                }
            }
            // a flow's own rules, and the nonce its signed records spend; another flow is
            // read only by the one rule that asks (a finish naming its source), never for
            // each Flow line
            EventType::Flow { quote_hash, step } => {
                let existing = self.store().flow(quote_hash);
                types::Flow::admits(
                    *quote_hash,
                    existing.as_ref(),
                    step,
                    |source_tx, arrival| self.other_arrivals_of_source(source_tx, arrival),
                )?;
                match step {
                    FlowStep::GaslessRelayed {
                        chain_id, nonce, ..
                    } => self.ensure_flow_nonce(TxPurpose::Relay(*quote_hash), *chain_id, *nonce),
                    FlowStep::CctpFinishSigned {
                        chain_id, nonce, ..
                    } => self.ensure_flow_nonce(TxPurpose::Finish(*quote_hash), *chain_id, *nonce),
                    FlowStep::TxClosed { .. } => Ok(()),
                }
            }
            // always legal; named rather than matched by `_` so a new variant has to be
            // classified here instead of silently defaulting to legal
            EventType::ConfigChanged { .. } | EventType::RolesChanged { .. } => Ok(()),
            // a final transaction is one the canister allocated a nonce for
            EventType::TxFinal {
                chain_id, nonce, ..
            } => self.ensure_allocated_nonce(*chain_id, *nonce),
            EventType::Swept { sweep } => Ok(crate::sweep::admits(self, sweep)?),
            EventType::SweepReverted { id, .. } => Ok(crate::sweep::admits_revert(self, *id)?),
        }
    }

    /// Whether a transaction may be created for `purpose`: its signed record's own rules,
    /// so a creation this guard admits is one the record will admit too, and its key must
    /// not already be holding a number it has not signed for. A cancel names no key.
    fn ensure_can_send(&self, purpose: &TxPurpose) -> Result<(), TransitionError> {
        let Some(quote_hash) = purpose.quote_hash() else {
            return Ok(());
        };
        // named rather than matched by `_`, so a new purpose has to be classified here
        // instead of falling into the rules of a swap's own leg
        match purpose {
            // a relay creates its flow with its signed record, and one quote is relayed at
            // most once (rule 4b.4 from this side; Swapic's `used(orderId)` is the other);
            // a finish is signed once per message and action the same way
            TxPurpose::Relay(_) | TxPurpose::Finish(_) => {
                if self.store().flow(&quote_hash).is_some() {
                    return Err(types::FlowError::Exists(quote_hash).into());
                }
            }
            // a cancel names no swap, so `quote_hash()` answered none and this function
            // returned before the match
            TxPurpose::Cancel(_) => return Ok(()),
            // a payout is signed only while it waits with its USDC reserved, and only once
            TxPurpose::PoolPayout(id) => {
                let payout = self
                    .store()
                    .pool_payout(*id)
                    .ok_or(types::pool::PoolStepError::UnknownPayout(*id))?;
                if payout.status != types::pool::PayoutStatus::Waiting
                    || !payout.base_reserved
                    || payout.order.is_some()
                {
                    return Err(types::pool::PoolStepError::PayoutNotFrom {
                        step: "TxCreated",
                        id: *id,
                        status: format!("{:?}", payout.status),
                    }
                    .into());
                }
            }
            // an orphan's refund only while it is held
            TxPurpose::PoolTransfer(hash) => {
                let held = self
                    .store()
                    .pool_orphans()
                    .into_iter()
                    .any(|(key, orphan)| {
                        types::pool::orphan_key(&key) == *hash
                            && orphan.status == types::pool::OrphanStatus::Held
                    });
                if !held {
                    return Err(types::pool::PoolStepError::Terms(
                        "no orphan held under that transfer's key",
                    )
                    .into());
                }
            }
            TxPurpose::PoolApprove => {}
            // a controller's claim of the canister's own deferred Across refunds: one at a time
            // by its key (below), whatever the chain holds back
            TxPurpose::AcrossClaim(_) => {}
            // an EVM sweep's one transaction: the sweep made on that chain and not taken back
            TxPurpose::Sweep(id) => {
                let sendable = self
                    .store()
                    .sweep(*id)
                    .is_some_and(|entry| !entry.reverted && entry.sweep.chain().is_some());
                if !sendable {
                    return Err(types::books::BooksError::NoSweep(*id).into());
                }
            }
            // one rescue per order
            TxPurpose::EcoRescue(order_id) => {
                if self
                    .store()
                    .eco_rescue(&types::hash::PoolRef::new(*order_id))
                    .is_some()
                {
                    return Err(types::pool::PoolStepError::Terms(
                        "the order's intent was rescued already",
                    )
                    .into());
                }
            }
            // a move's transaction only while its order is pinned, or its Eco fill landed for
            // its ck deposit: what `MoveSigned` admits
            TxPurpose::PoolMove(id) => {
                let item = self
                    .store()
                    .pool_move(*id)
                    .ok_or(types::pool::PoolStepError::UnknownMove(*id))?;
                let deposit = item.direction == types::pool::MoveDirection::BaseToIcp
                    && item.status == types::pool::MoveStatus::Landed
                    && item.route.is_some_and(|route| route.is_eco());
                if item.stuck.is_some()
                    || !(item.status == types::pool::MoveStatus::Routed || deposit)
                {
                    return Err(types::pool::PoolStepError::MoveNotFrom {
                        step: "TxCreated",
                        id: *id,
                        status: format!("{:?}", item.status),
                    }
                    .into());
                }
            }
        }
        // rule A4 from the key's side: two sends for ONE key that interleave at the
        // signature would otherwise both allocate, and the second's signed record would be
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

    /// A flow's signed record spends the number handed out for exactly its purpose.
    fn ensure_flow_nonce(
        &self,
        purpose: TxPurpose,
        chain_id: ChainId,
        nonce: Nonce,
    ) -> Result<(), TransitionError> {
        let held = self
            .store()
            .unsigned_nonce(&NonceKey { chain_id, nonce })
            .map(|unsigned| unsigned.purpose);
        if held != Some(purpose) {
            return Err(TransitionError::NonceNotUnsigned { chain_id, nonce });
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
    state.record_event(event.index);
    if event.payload.signed_tx().is_some() {
        state.record_signed(event.index);
    }
    match &event.payload {
        EventType::TxCreated {
            purpose,
            chain_id,
            nonce,
            action,
            ..
        } => state.record_nonce_allocated(*chain_id, *nonce, *purpose, event.timestamp, *action),
        EventType::TxCancelled {
            chain_id, nonce, ..
        } => state.record_nonce_spent(*chain_id, *nonce),
        EventType::Flow { quote_hash, step } => {
            // a signed step spends the nonce its `TxCreated` handed out, and takes the gas
            // action that line recorded, which a replacement is held to
            let mut action = None;
            if let FlowStep::GaslessRelayed {
                chain_id, nonce, ..
            }
            | FlowStep::CctpFinishSigned {
                chain_id, nonce, ..
            } = step
            {
                action = state
                    .store()
                    .unsigned_nonce(&NonceKey {
                        chain_id: *chain_id,
                        nonce: *nonce,
                    })
                    .and_then(|unsigned| unsigned.action);
                state.record_nonce_spent(*chain_id, *nonce);
            }
            state.record_flow(*quote_hash, step, event.index, event.timestamp, action);
        }
        EventType::Books { chain_id, step } => state.record_books(*chain_id, step, event.timestamp),
        EventType::Pool { step } => {
            // a signed pool step spends the nonce its `TxCreated` handed out
            if let Some((chain_id, nonce, _, _)) = step.signed_tx() {
                state.record_nonce_spent(chain_id, nonce);
            }
            state.record_pool(step, event.timestamp, event.index)
        }
        // a re-send allocates nothing and closes nothing; a flow's is indexed, so the
        // replacement that lands finds its flow before the flow closes
        EventType::TxReplaced {
            purpose, tx_hash, ..
        } => state.record_flow_replaced(*purpose, *tx_hash),
        // lines for deploy-time truth that lives in its own stable cell
        EventType::ConfigChanged { .. } | EventType::RolesChanged { .. } => {}
        // a final transaction's own record is the outbox's; the line is the log's
        EventType::TxFinal { .. } => {}
        EventType::Swept { sweep } => state.record_sweep(sweep),
        EventType::SweepReverted { id, .. } => state.record_sweep_reverted(*id),
    }
}

/// Why a log does not fold, in a test's heap fold. The replay stopped at `index` and applied
/// nothing from there.
#[cfg(test)]
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum ReplayError {
    #[error("event {found} sits where event {expected} belongs")]
    OutOfSequence {
        expected: types::EventIndex,
        found: types::EventIndex,
    },
    #[error("event {index} has no successor index")]
    LogFull { index: types::EventIndex },
    #[error("event {index} is refused: {error}")]
    Refused {
        index: types::EventIndex,
        error: TransitionError,
    },
}

/// Folds a log into a fresh heap state, admitting each event the way `append_event` did:
/// in sequence and through [`State::check_at`]. The tests' check that the stable fold is the
/// fold of its log.
#[cfg(test)]
pub fn replay(
    events: impl IntoIterator<Item = Event>,
) -> Result<State<crate::state::MemoryStore>, ReplayError> {
    let mut state = State::default();
    for event in events {
        let meta = state.meta();
        let index = event.index;
        if index != meta.next_event_index {
            return Err(ReplayError::OutOfSequence {
                expected: meta.next_event_index,
                found: index,
            });
        }
        if index.next().is_none() {
            return Err(ReplayError::LogFull { index });
        }
        state
            .check_at(&event.payload, event.timestamp)
            .map_err(|error| ReplayError::Refused { index, error })?;
        apply_state_transition(&mut state, &event);
    }
    Ok(state)
}

#[cfg(test)]
pub(crate) mod tests;
