//! A payout's progress steps (its ledger calls asked, answered, lost or refused): their rules, and what they change.

use super::*;

impl PoolLedger {
    /// The rules of a payout's progress (a deposit's, an ICP withdrawal's, or a Base
    /// payout's ICP legs): its transfer, its ICPSwap legs, its failures.
    pub(super) fn admits_progress(
        &self,
        step: &PoolStep,
        lookup: &impl PoolLookup,
    ) -> Result<(), PoolStepError> {
        match step {
            PoolStep::FellBack { r#ref, .. } => {
                let target = self
                    .target(r#ref, lookup)
                    .ok_or(PoolStepError::Unknown(*r#ref))?;
                let payout = target.payout().ok_or(PoolStepError::Mismatch)?;
                let path = payout.attempt.map(|attempt| attempt.path);
                if !target.in_payout() || !path.is_some_and(|path| path.swaps()) {
                    return Err(PoolStepError::Terms(
                        "only an ICPSwap leg with no output falls back",
                    ));
                }
                // a path through ICP whose first leg paid out falls back once the way back
                // brought the ICP back as ckUSDC, and never once its second leg paid out
                if payout.swap.out.is_some() {
                    let through_icp = matches!(path, Some(PayoutPath::IcpswapVia { .. }));
                    let second = payout.leg(Some(crate::pool::LEG_SECOND));
                    let back = payout.leg(Some(crate::pool::LEG_BACK));
                    if !through_icp || second.out.is_some() || back.out.is_none() {
                        return Err(PoolStepError::Terms(
                            "only an ICPSwap leg with no output falls back",
                        ));
                    }
                    return Ok(());
                }
                // a leg falls back before its swap, or after one that did not happen and was
                // undone (the driver read the pool to know): never once the swap paid out
                if payout.swap.begun && payout.swap.undone.is_none() {
                    return Err(PoolStepError::Terms("a begun swap falls back once undone"));
                }
                Ok(())
            }
            // a payout's transfer asked: never on other terms while one may have landed
            PoolStep::TransferAsked {
                r#ref,
                token,
                amount,
                ..
            } => {
                let target = self
                    .target(r#ref, lookup)
                    .ok_or(PoolStepError::Unknown(*r#ref))?;
                if !target.in_payout() {
                    return Err(PoolStepError::Terms("only a payout in progress asks"));
                }
                // may have landed: not refused at its last ask, or an answer of it once lost
                match target.payout() {
                    Some(payout)
                        if !payout.cannot_land_twice()
                            && payout.sent.is_some_and(|sent| {
                                (sent.token, sent.amount) != (*token, *amount)
                            }) =>
                    {
                        Err(PoolStepError::AskedOnOtherTerms)
                    }
                    _ => Ok(()),
                }
            }
            PoolStep::TransferLost { r#ref, .. } => {
                let target = self
                    .target(r#ref, lookup)
                    .ok_or(PoolStepError::Unknown(*r#ref))?;
                if !target.in_payout() {
                    return Err(PoolStepError::Terms(
                        "only a payout in progress loses an answer",
                    ));
                }
                match target.payout().and_then(|payout| payout.sent) {
                    Some(_) => Ok(()),
                    None => Err(PoolStepError::Terms(
                        "only a transfer asked loses an answer",
                    )),
                }
            }
            PoolStep::TransferRefused { r#ref } => {
                let target = self
                    .target(r#ref, lookup)
                    .ok_or(PoolStepError::Unknown(*r#ref))?;
                match target.payout().and_then(|payout| payout.sent) {
                    Some(sent) if !sent.refused => Ok(()),
                    _ => Err(PoolStepError::Terms("only a transfer asked is refused")),
                }
            }
            PoolStep::AttemptFailed { r#ref, .. } => {
                let target = self
                    .target(r#ref, lookup)
                    .ok_or(PoolStepError::Unknown(*r#ref))?;
                // a queued payout whose drive fails before it starts is counted too, and held
                // for a human at the usual attempts: it never blocks the queue
                if self.queue.0.iter().any(|(queued, _)| queued == r#ref) {
                    return Ok(());
                }
                if !target.in_payout() {
                    return Err(PoolStepError::Terms("only a payout in progress fails"));
                }
                Ok(())
            }
            PoolStep::SwapDeclined { r#ref, leg, .. } | PoolStep::SwapBegun { r#ref, leg, .. } => {
                let target = self
                    .target(r#ref, lookup)
                    .ok_or(PoolStepError::Unknown(*r#ref))?;
                let payout = target.payout().ok_or(PoolStepError::Mismatch)?;
                let swaps = admits_leg(payout, *leg);
                let this = payout.leg(*leg);
                if !target.in_payout() || !swaps || this.begun {
                    return Err(PoolStepError::Terms(
                        "the ICPSwap leg is not before its call",
                    ));
                }
                if matches!(step, PoolStep::SwapDeclined { .. }) && this.declined > 0 {
                    return Err(PoolStepError::Terms(
                        "a declined leg falls back the second time",
                    ));
                }
                Ok(())
            }
            PoolStep::Swapped { r#ref, leg, .. } => {
                let target = self
                    .target(r#ref, lookup)
                    .ok_or(PoolStepError::Unknown(*r#ref))?;
                let payout = target.payout().ok_or(PoolStepError::Mismatch)?;
                let this = payout.leg(*leg);
                if !this.begun || this.out.is_some() {
                    return Err(PoolStepError::Terms("only a begun swap answers once"));
                }
                Ok(())
            }
            PoolStep::SwapApproved { r#ref, leg } | PoolStep::SwapUndone { r#ref, leg, .. } => {
                let target = self
                    .target(r#ref, lookup)
                    .ok_or(PoolStepError::Unknown(*r#ref))?;
                let payout = target.payout().ok_or(PoolStepError::Mismatch)?;
                let swaps = admits_leg(payout, *leg);
                let this = payout.leg(*leg);
                if !target.in_payout() || !swaps || this.out.is_some() {
                    return Err(PoolStepError::Terms("the ICPSwap leg has its output"));
                }
                if matches!(step, PoolStep::SwapUndone { .. })
                    && (!this.begun || this.undone.is_some())
                {
                    return Err(PoolStepError::Terms("only a begun swap is undone, once"));
                }
                Ok(())
            }
            _ => Err(unrouted(step)),
        }
    }

    /// What a payout's progress changes (a deposit's, an ICP withdrawal's, or a Base payout's
    /// ICP legs): its transfer, its ICPSwap legs, its failures.
    pub(super) fn apply_progress(
        &mut self,
        step: &PoolStep,
        lookup: &impl PoolLookup,
        at: Timestamp,
        effects: &mut PoolEffects,
    ) {
        match step {
            PoolStep::FellBack {
                r#ref,
                why,
                amount,
                token,
            } => {
                let why = why.clone();
                let (amount, token) = (*amount, *token);
                effects.deposit = self.with_payout(r#ref, lookup, |payout| {
                    if let Some(attempt) = payout.attempt.as_mut() {
                        attempt.path = PayoutPath::Ckusdc;
                        attempt.amount = amount;
                        attempt.token = token;
                    }
                    payout.fell_back = Some(why);
                });
            }
            PoolStep::TransferAsked {
                r#ref,
                token,
                amount,
                spent,
            } => {
                effects.deposit = self.with_payout(r#ref, lookup, |payout| {
                    // an answer lost once is kept for good
                    let lost = payout.sent.map_or(Default::default(), |sent| sent.lost);
                    payout.sent = Some(crate::pool::Sent {
                        token: *token,
                        amount: *amount,
                        spent: *spent,
                        refused: false,
                        lost,
                    });
                });
            }
            PoolStep::TransferRefused { r#ref } => {
                effects.deposit = self.with_payout(r#ref, lookup, |payout| {
                    if let Some(sent) = payout.sent.as_mut() {
                        sent.refused = true;
                    }
                });
            }
            PoolStep::TransferLost { r#ref, held } => {
                effects.deposit = self.with_payout(r#ref, lookup, |payout| {
                    if let Some(sent) = payout.sent.as_mut() {
                        sent.lost = crate::pool::payout::Flag(true);
                    }
                    // held for a human at once, as its tenth failure would
                    if let Some(why) = held {
                        payout.failed = Some(why.clone());
                    }
                });
                if let (Some(why), Some((_, deposit))) = (held, effects.deposit.as_mut()) {
                    deposit.status = DepositStatus::PayoutFailed(why.clone());
                }
            }
            PoolStep::AttemptFailed { r#ref, why } => {
                let why = why.clone();
                let mut stopped = false;
                effects.deposit = self.with_payout(r#ref, lookup, |payout| {
                    payout.failures = payout.failures.saturating_add(1);
                    if payout.failures >= MAX_PAYOUT_ATTEMPTS {
                        payout.failed = Some(why.clone());
                        stopped = true;
                    }
                });
                // a stopped payout stays in `paying`: it is still between its start and its
                // record of payment, which the ICP pocket's check leaves out
                if stopped {
                    if let Some((_, deposit)) = effects.deposit.as_mut() {
                        deposit.status = DepositStatus::PayoutFailed(why);
                    }
                    // one that never started leaves the queue, so the payouts behind it go
                    // on; what it holds stays owed and was never reserved
                    let head = self.queue.head().map(|(head, _)| head);
                    self.queue.0.retain(|(queued, _)| queued != r#ref);
                    if head == Some(*r#ref) {
                        self.head_since = (!self.queue.0.is_empty()).then_some(at);
                    }
                }
            }
            PoolStep::SwapDeclined { r#ref, leg, .. } => {
                let leg = *leg;
                effects.deposit = self.with_payout(r#ref, lookup, |payout| {
                    let this = payout.leg_mut(leg);
                    this.declined = this.declined.saturating_add(1);
                });
            }
            PoolStep::SwapBegun {
                r#ref,
                before,
                leg,
                amount_in,
            } => {
                let (before, leg, amount_in) = (*before, *leg, *amount_in);
                effects.deposit = self.with_payout(r#ref, lookup, |payout| {
                    let this = payout.leg_mut(leg);
                    this.begun = true;
                    this.before = before;
                    this.amount_in = amount_in;
                });
            }
            PoolStep::Swapped {
                r#ref,
                out,
                send,
                leg,
            } => {
                let (out, send, leg) = (*out, *send, *leg);
                effects.deposit = self.with_payout(r#ref, lookup, |payout| {
                    let this = payout.leg_mut(leg);
                    this.out = Some(out);
                    this.send = send;
                });
            }
            PoolStep::SwapApproved { r#ref, leg } => {
                let leg = *leg;
                effects.deposit = self.with_payout(r#ref, lookup, |payout| {
                    let this = payout.leg_mut(leg);
                    this.approvals = Some(this.approvals.unwrap_or(0).saturating_add(1));
                });
            }
            PoolStep::SwapUndone {
                r#ref,
                cost,
                withdrawn,
                leg,
            } => {
                let (cost, withdrawn, leg) = (*cost, *withdrawn, *leg);
                effects.deposit = self.with_payout(r#ref, lookup, |payout| {
                    let this = payout.leg_mut(leg);
                    this.undone = Some(cost);
                    this.withdrawn = Some(withdrawn);
                });
            }
            _ => unrouted_apply(step),
        }
    }
}
