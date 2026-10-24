//! The Base payouts' rules (P11b): what each payout step must meet, read off the fold alone,
//! and what it moves. Each pocket move is refused in `admits` before `apply` could clamp it.
//!
//! - Opened: an ICP swap with nothing booked yet; a withdrawal burns its shares and spends its
//!   nonce.
//! - AmountKnown: `owed` gains the payout (and a withdrawal's network fee, held on Base with
//!   it); an ICP swap's ckUSDC is held in `icp.reserved` but its network fee, which goes to
//!   the ICP fee line.
//! - Queued / Waiting: first in first out; Waiting reserves the Base hold (`free -> reserved`).
//! - Signed: the order's amount `reserved -> in_flight`, once: a payout is never signed
//!   again (2026-10-23). Reverted: back to `reserved`, undone once Swapic says its order
//!   never ran.
//! - Paid: `in_flight` out, a withdrawal's fee to the Base fee line, an ICP swap's ckUSDC
//!   freed to the pool, `owed` down. An order that published an Eco intent (2026-10-23) is
//!   Delivering at its close, every line as it was, until its fill is read past its route
//!   deadline: Delivered is then the paid line; Unfilled keeps its USDC in `in_flight`, owed
//!   back by Eco, a withdrawal's fee back to `free`, and the payout is undone next. Eco's
//!   refund (Refunded) credits that USDC to `free` once, in either order, and never pays the
//!   payout again.
//! - Undone: everything released, an ICP swap's ckUSDC out with its refund, a withdrawal's
//!   shares minted back.
//! - Resolved (a controller, `resolve_payout`, 2026-10-23): a payout stopped for a human is
//!   put back on its undo (`Refund`), or its signed order found paid (`MarkPaid`): the paid
//!   line, from where a reverted payout's USDC sits (reserved again), once.
//!
//! Old semantics the fold still reads (before the payout plan flow, 2026-10-23): an ICP swap
//! opened with its user's own premium and least ckUSDC (no plan); a reverted payout waiting
//! with its order pinned to be signed again; a `Paid` payout with an Eco intent, which an Eco
//! refund reopened, its ckUSDC released (`icp_released`). hfisl's real log (2,310 events,
//! read 2026-10-22) holds no Pool event, so no such state exists on the deployed canister: no
//! migration. Each place is marked "old semantics".

#[cfg(test)]
mod tests;

use super::{credit_shares, sat_add, sat_sub, PoolEffects, PoolLedger, PoolLookup, PoolStepError};
use crate::numeric::{Timestamp, TokenAmount};
use crate::pool::payout::{
    EvmPayout, IcpSwapTerms, NonceUse, PayoutKind, PayoutLines, PayoutResolution, PayoutStatus,
    PinnedOrder, Reclaim, Undo,
};
use crate::pool::plan;
use crate::pool::{PoolAccount, PoolStep, ShareLine, Shares};

fn status_of(payout: &EvmPayout) -> String {
    format!("{:?}", payout.status)
}

/// The payout `id`, or why there is none.
fn payout_of(lookup: &impl PoolLookup, id: u64) -> Result<EvmPayout, PoolStepError> {
    lookup.payout(id).ok_or(PoolStepError::UnknownPayout(id))
}

/// A payout's step refused from the status the payout is in.
fn not_from(step: &'static str, payout: &EvmPayout) -> PoolStepError {
    PoolStepError::PayoutNotFrom {
        step,
        id: payout.id,
        status: status_of(payout),
    }
}

/// What a payout's paid line moves: its USDC out of the Base pocket's `in_flight`, a
/// withdrawal's fee to the Base fee line, `owed` down, an ICP swap's ckUSDC freed to the pool
/// once (a payout whose ckUSDC went free before never frees it again); and it is no longer
/// open.
fn paid(ledger: &mut PoolLedger, lines: &mut PayoutLines, payout: &mut EvmPayout, id: &u64) {
    let amount = payout.amount.unwrap_or(TokenAmount::ZERO);
    let fee = payout.base_fee.unwrap_or(TokenAmount::ZERO);
    let base = &mut ledger.pockets.base;
    base.in_flight = sat_sub(base.in_flight, amount);
    base.reserved = sat_sub(base.reserved, fee);
    lines.fees_base = sat_add(lines.fees_base, fee);
    ledger.owed = sat_sub(ledger.owed, payout.owed());
    if !payout.icp_released.0 {
        let held = payout.icp_hold();
        let icp = &mut ledger.pockets.icp;
        icp.reserved = sat_sub(icp.reserved, held);
        icp.free = sat_add(icp.free, held);
    }
    lines.open.remove(id);
    payout.status = PayoutStatus::Paid;
    payout.base_reserved = false;
    payout.icp_released = crate::pool::payout::Flag(
        payout.icp_released.0 || (payout.is_icp_swap() && payout.received.is_some()),
    );
}

/// An Eco intent never filled: the payout is undone next; its USDC stays in the Base
/// pocket's `in_flight`, owed back by Eco; a withdrawal's fee goes back to `free`, as any
/// undo gives it back.
fn unfilled(ledger: &mut PoolLedger, payout: &mut EvmPayout) {
    let fee = payout.base_fee.unwrap_or(TokenAmount::ZERO);
    let base = &mut ledger.pockets.base;
    base.reserved = sat_sub(base.reserved, fee);
    base.free = sat_add(base.free, fee);
    payout.base_reserved = false;
    payout.status = PayoutStatus::Unfilled;
    payout.reclaim = Some(Reclaim::Pending);
}

/// Takes a payout off the queue at `at`, with its hold.
fn dequeue(
    lines: &mut PayoutLines,
    payout: &mut EvmPayout,
    effects: &mut PoolEffects,
    at: Timestamp,
) {
    if let Some(seq) = payout.queued_at.take() {
        effects.dequeued = Some(seq);
        lines.queue_len = lines.queue_len.saturating_sub(1);
        lines.queued = sat_sub(lines.queued, payout.base_hold());
        lines.head_since = (lines.queue_len > 0).then_some(at);
    }
}

impl PoolLedger {
    /// A planned ICP swap's opening (2026-10-23): its plan, its slippage and the plan's
    /// amount together; a slippage of at most 500 bps; a plan that is an order of exactly
    /// that amount; a premium taken only once the amount is known; a ckUSDC swap's least
    /// ckUSDC all of it; and the Base pocket able to pay it behind an empty queue: the plan's
    /// amount and 5%, and a ckUSDC swap's own payout, exact already. A swap with none of the
    /// three opened before the plan flow.
    fn admits_plan(
        &self,
        swap: &IcpSwapTerms,
        plan: Option<&PinnedOrder>,
    ) -> Result<(), PoolStepError> {
        let (plan, slippage, planned) = match (plan, swap.slippage_bps, swap.planned) {
            // old semantics: a swap opened before the plan flow (see the module's note)
            (None, None, None) => return Ok(()),
            (Some(plan), Some(slippage), Some(planned)) => (plan, slippage, planned),
            _ => return Err(PoolStepError::Mismatch),
        };
        if slippage > plan::MAX_SLIPPAGE_BPS {
            return Err(PoolStepError::Terms("a slippage above 500 bps"));
        }
        if planned == TokenAmount::ZERO {
            return Err(PoolStepError::Zero("planned"));
        }
        let order = crate::swapic::decode_order(&plan.order).ok_or(PoolStepError::Terms(
            "the plan is not abi.encode of an order",
        ))?;
        if TokenAmount::from_be_bytes(order.amountIn.to_be_bytes::<32>()) != planned {
            return Err(PoolStepError::Terms(
                "the plan's order spends another amount",
            ));
        }
        if swap.premium != TokenAmount::ZERO {
            return Err(PoolStepError::Terms(
                "a planned swap's premium is taken once its amount is known",
            ));
        }
        let settings = self.settings()?;
        let mut need = plan::open_cover(planned).ok_or(PoolStepError::Overflow)?;
        if swap.token_in == settings.ckusdc_ledger {
            if swap.min_usdc != swap.amount_in {
                return Err(PoolStepError::Terms(
                    "a ckUSDC swap's least ckUSDC is all of it",
                ));
            }
            let payout = plan::payout_of(settings, swap.amount_in, swap.network_fee)
                .ok_or(PoolStepError::Terms("the fees take all of the swap"))?;
            need = need.max(payout);
        }
        if self.lines().queue_len > 0 || need > self.pockets.base.free {
            return Err(PoolStepError::BaseShort {
                free: self.pockets.base.free,
                need,
            });
        }
        Ok(())
    }

    /// What the Base pocket's free USDC covers behind the queue.
    pub fn base_available(&self) -> TokenAmount {
        sat_sub(self.pockets.base.free, self.lines().queued)
    }

    /// The rules of the Base payouts' steps and of the orphans'.
    pub(super) fn admits_payout(
        &self,
        step: &PoolStep,
        lookup: &impl PoolLookup,
    ) -> Result<(), PoolStepError> {
        match step {
            PoolStep::PayoutSettingsSet { settings } => Ok(settings.validate()?),
            PoolStep::PayoutOpened { .. }
            | PoolStep::PayoutAmountKnown { .. }
            | PoolStep::PayoutPulled { .. } => self.admits_payout_opening(step, lookup),
            PoolStep::PayoutQueued { .. }
            | PoolStep::PayoutWaiting { .. }
            | PoolStep::PayoutSigned { .. }
            | PoolStep::PayoutFitWaited { .. }
            | PoolStep::PayoutPaid { .. }
            | PoolStep::PayoutReverted { .. }
            | PoolStep::PayoutRelisted { .. } => self.admits_payout_order(step, lookup),
            PoolStep::PayoutFillEarly { .. }
            | PoolStep::PayoutDelivered { .. }
            | PoolStep::PayoutUnfilled { .. }
            | PoolStep::PayoutRefunded { .. } => self.admits_payout_delivery(step, lookup),
            PoolStep::PayoutResolved { .. }
            | PoolStep::PayoutStopped { .. }
            | PoolStep::PayoutUndoing { .. }
            | PoolStep::PayoutUndone { .. } => self.admits_payout_undo(step, lookup),
            PoolStep::OrphanBooked { .. }
            | PoolStep::OrphanRefundSigned { .. }
            | PoolStep::TermsClaimed { .. }
            | PoolStep::OrphanRefundClosed { .. } => self.admits_orphan(step, lookup),
            _ => Ok(()),
        }
    }

    /// The rules of a payout's opening: opened, an ICP swap's pull, its amount known.
    fn admits_payout_opening(
        &self,
        step: &PoolStep,
        lookup: &impl PoolLookup,
    ) -> Result<(), PoolStepError> {
        let lines = self.lines();
        match step {
            PoolStep::PayoutOpened {
                id,
                kind,
                terms,
                undo,
                plan,
            } => {
                self.settings()?;
                lines.settings.ok_or(PoolStepError::NoPayoutSettings)?;
                if *id != lines.next_id || lookup.payout(*id).is_some() {
                    return Err(PoolStepError::PayoutId {
                        expected: lines.next_id,
                    });
                }
                if terms.min_out == TokenAmount::ZERO {
                    return Err(PoolStepError::Zero("min_out"));
                }
                match kind.as_ref() {
                    PayoutKind::IcpSwap(swap) => {
                        if swap.amount_in == TokenAmount::ZERO {
                            return Err(PoolStepError::Zero("amount_in"));
                        }
                        let fees = swap
                            .premium
                            .checked_add(swap.network_fee)
                            .ok_or(PoolStepError::Overflow)?;
                        if fees >= swap.min_usdc {
                            return Err(PoolStepError::Terms(
                                "the premium and the network fee must be below min_usdc",
                            ));
                        }
                        if *undo != (Undo::Refund { to: swap.refund_to }) {
                            return Err(PoolStepError::Mismatch);
                        }
                        self.admits_plan(swap, plan.as_deref())?;
                    }
                    PayoutKind::Withdrawal(withdrawal) => {
                        if plan.is_some() {
                            return Err(PoolStepError::Mismatch);
                        }
                        withdrawal.payout_amount().ok_or(PoolStepError::Terms(
                            "the rebalancing share and the network fee take all of the withdrawal",
                        ))?;
                        let used = NonceUse {
                            staker: withdrawal.staker,
                            nonce: withdrawal.nonce,
                        };
                        if lookup.nonce_used(&used) {
                            return Err(PoolStepError::NonceUsed(withdrawal.nonce));
                        }
                        let owner = PoolAccount::Evm(withdrawal.staker);
                        let have = lookup.shares_of(&owner.key());
                        if have < withdrawal.burned {
                            return Err(PoolStepError::ShortShares {
                                have,
                                need: withdrawal.burned,
                            });
                        }
                        if withdrawal.burned > withdrawal.max_shares {
                            return Err(PoolStepError::Terms("the burn is above max_shares"));
                        }
                        let least = Shares::burn_for(
                            withdrawal.usdc,
                            self.total_assets(),
                            self.total_shares,
                        )
                        .ok_or(PoolStepError::Terms("the pool's assets cannot pay that"))?;
                        if withdrawal.burned < least {
                            return Err(PoolStepError::BurnTooSmall {
                                burned: withdrawal.burned,
                                least,
                                usdc: withdrawal.usdc,
                            });
                        }
                        let remint = Undo::Remint {
                            owner,
                            shares: withdrawal.burned,
                        };
                        if *undo != remint {
                            return Err(PoolStepError::Mismatch);
                        }
                    }
                }
                Ok(())
            }
            PoolStep::PayoutAmountKnown {
                id,
                amount,
                received,
            } => {
                let payout = payout_of(lookup, *id)?;
                if payout.amount.is_some() {
                    return Err(not_from("PayoutAmountKnown", &payout));
                }
                if *amount == TokenAmount::ZERO {
                    return Err(PoolStepError::Zero("amount"));
                }
                match (payout.kind, received) {
                    (PayoutKind::IcpSwap(swap), Some(received)) => {
                        if !matches!(payout.status, PayoutStatus::Opened | PayoutStatus::Swapping) {
                            return Err(not_from("PayoutAmountKnown", &payout));
                        }
                        if *received < swap.min_usdc {
                            return Err(PoolStepError::Terms("the ckUSDC is below min_usdc"));
                        }
                        // a planned swap's premium is the settings' share of what it made; old
                        // semantics: an unplanned one's is its user's own (the module's note)
                        let expected = if swap.is_planned() {
                            plan::payout_of(self.settings()?, *received, swap.network_fee)
                        } else {
                            received
                                .checked_sub(swap.premium)
                                .and_then(|left| left.checked_sub(swap.network_fee))
                        };
                        if expected != Some(*amount) {
                            return Err(PoolStepError::Terms(
                                "the amount is not the ckUSDC less the premium and the network fee",
                            ));
                        }
                    }
                    (PayoutKind::Withdrawal(withdrawal), None) => {
                        if payout.status != PayoutStatus::Opened {
                            return Err(not_from("PayoutAmountKnown", &payout));
                        }
                        if withdrawal.payout_amount() != Some(*amount) {
                            return Err(PoolStepError::Terms(
                                "the amount is not the withdrawal less its rebalancing share and network fee",
                            ));
                        }
                    }
                    _ => return Err(PoolStepError::Mismatch),
                }
                Ok(())
            }
            // the pull of an ICP swap opened and not pulled yet
            PoolStep::PayoutPulled { id, attempt, .. } => {
                let payout = payout_of(lookup, *id)?;
                if payout.status != PayoutStatus::Opened || !payout.is_icp_swap() {
                    return Err(not_from("PayoutPulled", &payout));
                }
                if attempt.is_some_and(|attempt| !attempt.path.swaps()) {
                    return Err(PoolStepError::Terms("a pull's legs swap on ICPSwap"));
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// The rules of a payout's order: queued, waiting, signed once, its transaction's end.
    fn admits_payout_order(
        &self,
        step: &PoolStep,
        lookup: &impl PoolLookup,
    ) -> Result<(), PoolStepError> {
        let lines = self.lines();
        match step {
            PoolStep::PayoutQueued { id } => {
                let payout = payout_of(lookup, *id)?;
                if payout.status != PayoutStatus::Known {
                    return Err(not_from("PayoutQueued", &payout));
                }
                Ok(())
            }
            // first in first out: a new payout waits only behind an empty queue, a queued one
            // only from the head; and only once the free USDC covers its whole hold
            PoolStep::PayoutWaiting { id } => {
                let payout = payout_of(lookup, *id)?;
                let need = payout.base_hold();
                match payout.status {
                    PayoutStatus::Known if lines.queue_len > 0 => {
                        return Err(PoolStepError::BaseShort {
                            free: self.base_available(),
                            need,
                        })
                    }
                    PayoutStatus::Known => {}
                    PayoutStatus::Queued => {
                        if lookup.queue_head().map(|(_, head)| head) != Some(*id) {
                            return Err(PoolStepError::Terms(
                                "a payout leaves the queue first in first out",
                            ));
                        }
                    }
                    _ => return Err(not_from("PayoutWaiting", &payout)),
                }
                if need > self.pockets.base.free {
                    return Err(PoolStepError::BaseShort {
                        free: self.pockets.base.free,
                        need,
                    });
                }
                Ok(())
            }
            // a payout waiting is signed once (2026-10-23): a revert undoes it, never signs it
            // again, its pinned order included
            PoolStep::PayoutSigned { id, .. } => {
                let payout = payout_of(lookup, *id)?;
                if payout.status != PayoutStatus::Waiting
                    || !payout.base_reserved
                    || payout.order.is_some()
                {
                    return Err(not_from("PayoutSigned", &payout));
                }
                let amount = payout.amount.ok_or(PoolStepError::Mismatch)?;
                if amount > self.pockets.base.reserved {
                    return Err(PoolStepError::Overflow);
                }
                Ok(())
            }
            // a planned swap's try that waits: while it waits with its plan, never signed, at
            // most `MAX_FIT_TRIES` times
            PoolStep::PayoutFitWaited { id, .. } => {
                let payout = payout_of(lookup, *id)?;
                if payout.status != PayoutStatus::Waiting
                    || payout.plan.is_none()
                    || payout.order.is_some()
                    || payout.fit_tries.unwrap_or(0) >= crate::pool::payout::MAX_FIT_TRIES
                {
                    return Err(not_from("PayoutFitWaited", &payout));
                }
                Ok(())
            }
            PoolStep::PayoutPaid { id, .. } | PoolStep::PayoutReverted { id, .. } => {
                let payout = payout_of(lookup, *id)?;
                if !matches!(payout.status, PayoutStatus::Signed(_)) {
                    return Err(not_from(step.name(), &payout));
                }
                let amount = payout.amount.ok_or(PoolStepError::Mismatch)?;
                if amount > self.pockets.base.in_flight {
                    return Err(PoolStepError::Overflow);
                }
                Ok(())
            }
            PoolStep::PayoutRelisted { id } => {
                let payout = payout_of(lookup, *id)?;
                if payout.status != PayoutStatus::Waiting {
                    return Err(not_from("PayoutRelisted", &payout));
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// The rules of an Eco intent's delivery: its fill read, and Eco's refund.
    fn admits_payout_delivery(
        &self,
        step: &PoolStep,
        lookup: &impl PoolLookup,
    ) -> Result<(), PoolStepError> {
        match step {
            // a delivering payout's fill read too early
            PoolStep::PayoutFillEarly { id } => {
                let payout = payout_of(lookup, *id)?;
                if payout.status != PayoutStatus::Delivering {
                    return Err(not_from("PayoutFillEarly", &payout));
                }
                Ok(())
            }
            // its Eco intent read past the route's deadline: filled, or never
            PoolStep::PayoutDelivered { id, .. } | PoolStep::PayoutUnfilled { id, .. } => {
                let payout = payout_of(lookup, *id)?;
                if payout.status != PayoutStatus::Delivering || payout.reclaim.is_some() {
                    return Err(not_from(step.name(), &payout));
                }
                let amount = payout.amount.ok_or(PoolStepError::Mismatch)?;
                let fee = payout.base_fee.unwrap_or(TokenAmount::ZERO);
                if amount > self.pockets.base.in_flight || fee > self.pockets.base.reserved {
                    return Err(PoolStepError::Overflow);
                }
                Ok(())
            }
            // Eco's refund of the intent the payout published, all of its reward: of a payout
            // delivering (the refund proves the intent unfilled), or one whose USDC Eco still
            // owes back; credited once, and never a payout paid again
            PoolStep::PayoutRefunded {
                id, credit, amount, ..
            } => {
                let payout = payout_of(lookup, *id)?;
                // old semantics: a `Paid` payout with an intent is never reopened now; its
                // refund is held as an orphan (the module's note)
                let owed_back = payout.status == PayoutStatus::Delivering
                    && payout.reclaim.is_none()
                    || payout.reclaim == Some(Reclaim::Pending);
                if !owed_back || payout.intent.is_none() {
                    return Err(not_from("PayoutRefunded", &payout));
                }
                if lookup.is_credited(credit) {
                    return Err(PoolStepError::AlreadyCredited(*credit));
                }
                if payout.amount != Some(*amount) {
                    return Err(PoolStepError::Terms(
                        "an Eco refund gives back exactly the payout's amount",
                    ));
                }
                let fee = payout.base_fee.unwrap_or(TokenAmount::ZERO);
                if *amount > self.pockets.base.in_flight
                    || (payout.status == PayoutStatus::Delivering
                        && fee > self.pockets.base.reserved)
                {
                    return Err(PoolStepError::Overflow);
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// The rules of a payout stopped for a human or undone: stopped, a controller's
    /// resolution, its undo.
    fn admits_payout_undo(
        &self,
        step: &PoolStep,
        lookup: &impl PoolLookup,
    ) -> Result<(), PoolStepError> {
        match step {
            // a controller's answer to a payout stopped for a human: its money back, or its
            // signed order found paid (never one whose Eco intent was read unfilled), once
            PoolStep::PayoutResolved { id, resolution, .. } => {
                let payout = payout_of(lookup, *id)?;
                if !matches!(payout.status, PayoutStatus::Failed(_)) {
                    return Err(not_from("PayoutResolved", &payout));
                }
                match resolution {
                    PayoutResolution::Refund => {
                        if let Undo::Remint { shares, .. } = payout.undo {
                            self.total_shares
                                .checked_add(shares)
                                .ok_or(PoolStepError::Overflow)?;
                        }
                        if payout.icp_released.0 && payout.icp_hold() > self.pockets.icp.free {
                            return Err(PoolStepError::PocketShort {
                                free: self.pockets.icp.free,
                                need: payout.icp_hold(),
                            });
                        }
                    }
                    PayoutResolution::MarkPaid => {
                        if payout.order.is_none() || !payout.base_reserved {
                            return Err(PoolStepError::Terms(
                                "only a payout whose order was signed, its USDC reserved, can have paid",
                            ));
                        }
                        if payout.reclaim.is_some() {
                            return Err(PoolStepError::Terms(
                                "an Eco intent read unfilled was never delivered",
                            ));
                        }
                        let amount = payout.amount.ok_or(PoolStepError::Mismatch)?;
                        let hold = amount
                            .checked_add(payout.base_fee.unwrap_or(TokenAmount::ZERO))
                            .ok_or(PoolStepError::Overflow)?;
                        if hold > self.pockets.base.reserved
                            || (!payout.icp_released.0
                                && payout.icp_hold() > self.pockets.icp.reserved)
                        {
                            return Err(PoolStepError::Overflow);
                        }
                    }
                }
                Ok(())
            }
            // what happened to its money is not known: before it waits for an order, or a
            // reverted order Swapic says ran after all
            PoolStep::PayoutStopped { id, .. } => {
                let payout = payout_of(lookup, *id)?;
                if !matches!(
                    payout.status,
                    PayoutStatus::Opened
                        | PayoutStatus::Swapping
                        | PayoutStatus::Undoing(_)
                        | PayoutStatus::Reverted
                ) {
                    return Err(not_from("PayoutStopped", &payout));
                }
                Ok(())
            }
            PoolStep::PayoutUndoing { id, .. } => {
                let payout = payout_of(lookup, *id)?;
                if !matches!(
                    payout.status,
                    PayoutStatus::Opened
                        | PayoutStatus::Swapping
                        | PayoutStatus::Known
                        | PayoutStatus::Queued
                        | PayoutStatus::Waiting
                        | PayoutStatus::Reverted
                        | PayoutStatus::Unfilled
                ) {
                    return Err(not_from("PayoutUndoing", &payout));
                }
                // old semantics (the module's note): an ICP swap paid once and reopened by an
                // Eco refund: its refund is held back out of the free ckUSDC before it is asked
                if payout.icp_released.0 && payout.icp_hold() > self.pockets.icp.free {
                    return Err(PoolStepError::PocketShort {
                        free: self.pockets.icp.free,
                        need: payout.icp_hold(),
                    });
                }
                Ok(())
            }
            // a signed payout waits for its transaction's end: undone only once it reverted
            PoolStep::PayoutUndone { id, .. } => {
                let payout = payout_of(lookup, *id)?;
                if !matches!(
                    payout.status,
                    PayoutStatus::Opened
                        | PayoutStatus::Swapping
                        | PayoutStatus::Known
                        | PayoutStatus::Queued
                        | PayoutStatus::Waiting
                        | PayoutStatus::Undoing(_)
                        | PayoutStatus::Reverted
                        | PayoutStatus::Unfilled
                ) {
                    return Err(not_from("PayoutUndone", &payout));
                }
                if let Undo::Remint { shares, .. } = payout.undo {
                    self.total_shares
                        .checked_add(shares)
                        .ok_or(PoolStepError::Overflow)?;
                }
                // old semantics (the module's note): an ICP swap paid once, then reopened by
                // Eco's refund: its ckUSDC is the free one
                if payout.icp_released.0 && payout.icp_hold() > self.pockets.icp.free {
                    return Err(PoolStepError::PocketShort {
                        free: self.pockets.icp.free,
                        need: payout.icp_hold(),
                    });
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// The rules of the transfers no deposit took: booked, refunded, claimed by their terms.
    fn admits_orphan(
        &self,
        step: &PoolStep,
        lookup: &impl PoolLookup,
    ) -> Result<(), PoolStepError> {
        match step {
            PoolStep::OrphanBooked { key } | PoolStep::OrphanRefundSigned { key, .. } => {
                let orphan = lookup
                    .orphan(key)
                    .ok_or(PoolStepError::Terms("no orphan under that key"))?;
                // held, or awaiting terms (the controller's door checks its time ran out)
                if !matches!(
                    orphan.status,
                    crate::pool::OrphanStatus::Held
                        | crate::pool::OrphanStatus::AwaitingTerms { .. }
                ) {
                    return Err(PoolStepError::Terms("an orphan is resolved once"));
                }
                if orphan.amount > self.unattributed {
                    return Err(PoolStepError::Overflow);
                }
                if let PoolStep::OrphanRefundSigned { sent, .. } = step {
                    if *sent == TokenAmount::ZERO || *sent > orphan.amount {
                        return Err(PoolStepError::Terms(
                            "a refund sends some of the orphan, never more",
                        ));
                    }
                }
                Ok(())
            }
            PoolStep::TermsClaimed { credit, refused } => {
                let orphan = lookup
                    .orphan(credit)
                    .ok_or(PoolStepError::Terms("no transfer awaits its terms there"))?;
                if !matches!(
                    orphan.status,
                    crate::pool::OrphanStatus::AwaitingTerms { .. }
                ) {
                    return Err(PoolStepError::Terms(
                        "only a transfer awaiting its terms is claimed",
                    ));
                }
                if refused.is_none() && orphan.amount > self.unattributed {
                    return Err(PoolStepError::Overflow);
                }
                Ok(())
            }
            PoolStep::OrphanRefundClosed { key, .. } => {
                let orphan = lookup
                    .orphan(key)
                    .ok_or(PoolStepError::Terms("no orphan under that key"))?;
                if !matches!(orphan.status, crate::pool::OrphanStatus::Refunding { .. }) {
                    return Err(PoolStepError::Terms("only a refund on its way closes"));
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// What a payout step changes, None for a step that is not one.
    pub(super) fn apply_payout(
        self,
        step: &PoolStep,
        lookup: &impl PoolLookup,
        at: Timestamp,
        index: u64,
    ) -> Option<PoolEffects> {
        let mut ledger = self;
        let mut lines = ledger.lines();
        let mut effects = PoolEffects::of(PoolLedger::default());
        match step {
            PoolStep::PayoutSettingsSet { settings } => lines.settings = Some(*settings),
            PoolStep::PayoutOpened { .. }
            | PoolStep::PayoutAmountKnown { .. }
            | PoolStep::PayoutPulled { .. } => {
                ledger.apply_payout_opening(step, lookup, at, &mut lines, &mut effects)?
            }
            PoolStep::PayoutQueued { .. }
            | PoolStep::PayoutWaiting { .. }
            | PoolStep::PayoutSigned { .. }
            | PoolStep::PayoutPaid { .. }
            | PoolStep::PayoutFitWaited { .. }
            | PoolStep::PayoutReverted { .. }
            | PoolStep::PayoutRelisted { .. } => {
                ledger.apply_payout_order(step, lookup, at, index, &mut lines, &mut effects)?
            }
            PoolStep::PayoutDelivered { .. }
            | PoolStep::PayoutFillEarly { .. }
            | PoolStep::PayoutUnfilled { .. }
            | PoolStep::PayoutRefunded { .. } => {
                ledger.apply_payout_delivery(step, lookup, at, &mut lines, &mut effects)?
            }
            PoolStep::PayoutResolved { .. }
            | PoolStep::PayoutStopped { .. }
            | PoolStep::PayoutUndoing { .. }
            | PoolStep::PayoutUndone { .. } => {
                ledger.apply_payout_undo(step, lookup, at, &mut lines, &mut effects)?
            }
            PoolStep::OrphanBooked { .. }
            | PoolStep::TermsClaimed { .. }
            | PoolStep::OrphanRefundSigned { .. }
            | PoolStep::OrphanRefundClosed { .. } => {
                ledger.apply_orphan(step, lookup, &mut lines, &mut effects)?
            }
            _ => return None,
        }
        ledger.payouts = Some(lines);
        effects.ledger = ledger;
        Some(effects)
    }

    /// What a payout's opening changes: opened, an ICP swap's pull, its amount known.
    fn apply_payout_opening(
        &mut self,
        step: &PoolStep,
        lookup: &impl PoolLookup,
        at: Timestamp,
        lines: &mut PayoutLines,
        effects: &mut PoolEffects,
    ) -> Option<()> {
        match step {
            PoolStep::PayoutOpened {
                id,
                kind,
                terms,
                undo,
                plan,
            } => {
                lines.next_id = id.saturating_add(1);
                lines.open.insert(*id);
                if let PayoutKind::Withdrawal(withdrawal) = kind.as_ref() {
                    let account = PoolAccount::Evm(withdrawal.staker);
                    let shares = lookup
                        .shares_of(&account.key())
                        .checked_sub(withdrawal.burned)
                        .unwrap_or(Shares::ZERO);
                    effects.shares = Some((account.key(), ShareLine { account, shares }));
                    self.total_shares = self
                        .total_shares
                        .checked_sub(withdrawal.burned)
                        .unwrap_or(Shares::ZERO);
                    effects.nonce = Some(NonceUse {
                        staker: withdrawal.staker,
                        nonce: withdrawal.nonce,
                    });
                }
                let mut payout = EvmPayout::new(*id, **kind, *terms, *undo, at);
                payout.plan = plan.as_deref().cloned();
                effects.payout = Some(payout);
            }
            PoolStep::PayoutAmountKnown {
                id,
                amount,
                received,
            } => {
                let mut payout = lookup.payout(*id)?;
                payout.amount = Some(*amount);
                payout.status = PayoutStatus::Known;
                match payout.kind {
                    PayoutKind::IcpSwap(_) => {
                        payout.received = *received;
                        let icp = &mut self.pockets.icp;
                        icp.reserved = sat_add(icp.reserved, payout.icp_hold());
                        lines.fees_icp = sat_add(lines.fees_icp, payout.icp_fee());
                    }
                    PayoutKind::Withdrawal(withdrawal) => {
                        payout.base_fee = Some(withdrawal.network_fee);
                    }
                }
                self.owed = sat_add(self.owed, payout.owed());
                effects.payout = Some(payout);
            }
            PoolStep::PayoutPulled { id, block, attempt } => {
                let mut payout = lookup.payout(*id)?;
                payout.status = PayoutStatus::Swapping;
                payout.icp_block = Some(*block);
                payout.legs = attempt.map(|attempt| crate::pool::PayoutProgress {
                    attempt: Some(attempt),
                    ..crate::pool::PayoutProgress::default()
                });
                effects.payout = Some(payout);
            }
            _ => return None,
        }
        Some(())
    }

    /// What a payout's order changes: queued, waiting, signed once, its transaction's end.
    fn apply_payout_order(
        &mut self,
        step: &PoolStep,
        lookup: &impl PoolLookup,
        at: Timestamp,
        index: u64,
        lines: &mut PayoutLines,
        effects: &mut PoolEffects,
    ) -> Option<()> {
        match step {
            PoolStep::PayoutQueued { id } => {
                let mut payout = lookup.payout(*id)?;
                let seq = lines.next_seq;
                lines.next_seq = seq.saturating_add(1);
                if lines.queue_len == 0 {
                    lines.head_since = Some(at);
                }
                lines.queue_len = lines.queue_len.saturating_add(1);
                lines.queued = sat_add(lines.queued, payout.base_hold());
                payout.queued_at = Some(seq);
                payout.status = PayoutStatus::Queued;
                effects.queued = Some((seq, *id));
                effects.payout = Some(payout);
            }
            PoolStep::PayoutWaiting { id } => {
                let mut payout = lookup.payout(*id)?;
                dequeue(lines, &mut payout, effects, at);
                let hold = payout.base_hold();
                let base = &mut self.pockets.base;
                base.free = sat_sub(base.free, hold);
                base.reserved = sat_add(base.reserved, hold);
                payout.base_reserved = true;
                payout.status = PayoutStatus::Waiting;
                payout.waiting_since = Some(at);
                effects.payout = Some(payout);
            }
            PoolStep::PayoutSigned {
                id, order, tx_hash, ..
            } => {
                let mut payout = lookup.payout(*id)?;
                let amount = payout.amount.unwrap_or(TokenAmount::ZERO);
                let base = &mut self.pockets.base;
                base.reserved = sat_sub(base.reserved, amount);
                base.in_flight = sat_add(base.in_flight, amount);
                payout.status = PayoutStatus::Signed(index);
                payout.order = Some(order.clone());
                payout.tx_hash = Some(*tx_hash);
                effects.payout = Some(payout);
            }
            // an order that published an Eco intent: delivering, every line as it was until
            // its fill is read
            PoolStep::PayoutPaid {
                id,
                tx_hash,
                block,
                intent: Some(intent),
            } => {
                let mut payout = lookup.payout(*id)?;
                payout.status = PayoutStatus::Delivering;
                payout.tx_hash = Some(*tx_hash);
                payout.paid_block = Some(*block);
                payout.intent = Some(*intent);
                effects.intent = Some((*intent, *id));
                effects.payout = Some(payout);
            }
            PoolStep::PayoutPaid {
                id,
                tx_hash,
                block,
                intent: None,
            } => {
                let mut payout = lookup.payout(*id)?;
                payout.tx_hash = Some(*tx_hash);
                payout.paid_block = Some(*block);
                paid(self, lines, &mut payout, id);
                effects.payout = Some(payout);
            }
            // counted, nothing moves
            PoolStep::PayoutFitWaited { id, .. } => {
                let mut payout = lookup.payout(*id)?;
                payout.fit_tries = Some(payout.fit_tries.unwrap_or(0).saturating_add(1));
                effects.payout = Some(payout);
            }
            // back to reserved, for its undo: never signed again
            PoolStep::PayoutReverted { id, block } => {
                let mut payout = lookup.payout(*id)?;
                payout.reverted_block = *block;
                payout.reverted_at = Some(at);
                let amount = payout.amount.unwrap_or(TokenAmount::ZERO);
                let base = &mut self.pockets.base;
                base.in_flight = sat_sub(base.in_flight, amount);
                base.reserved = sat_add(base.reserved, amount);
                payout.status = PayoutStatus::Reverted;
                effects.payout = Some(payout);
            }
            PoolStep::PayoutRelisted { id } => {
                let mut payout = lookup.payout(*id)?;
                payout.waiting_since = Some(at);
                effects.payout = Some(payout);
            }
            _ => return None,
        }
        Some(())
    }

    /// What an Eco intent's delivery changes: its fill read, and Eco's refund.
    fn apply_payout_delivery(
        &mut self,
        step: &PoolStep,
        lookup: &impl PoolLookup,
        at: Timestamp,
        lines: &mut PayoutLines,
        effects: &mut PoolEffects,
    ) -> Option<()> {
        match step {
            PoolStep::PayoutDelivered { id, .. } => {
                let mut payout = lookup.payout(*id)?;
                paid(self, lines, &mut payout, id);
                effects.payout = Some(payout);
            }
            // noted, nothing moves
            PoolStep::PayoutFillEarly { id } => {
                let mut payout = lookup.payout(*id)?;
                payout.fill_early_at = Some(at);
                effects.payout = Some(payout);
            }
            PoolStep::PayoutUnfilled { id, .. } => {
                let mut payout = lookup.payout(*id)?;
                unfilled(self, &mut payout);
                effects.payout = Some(payout);
            }
            // Eco's refund: its USDC back to the Base pocket's free line, once; a payout still
            // delivering is unfilled by it (undone next). Never a payout paid again
            PoolStep::PayoutRefunded {
                id,
                credit,
                amount,
                block,
            } => {
                let mut payout = lookup.payout(*id)?;
                if payout.status == PayoutStatus::Delivering {
                    unfilled(self, &mut payout);
                }
                let base = &mut self.pockets.base;
                base.in_flight = sat_sub(base.in_flight, *amount);
                base.free = sat_add(base.free, *amount);
                self.unseen.push(crate::pool::UnseenArrival {
                    chain: credit.chain,
                    block: *block,
                    amount: *amount,
                });
                effects.credited = Some(*credit);
                payout.reclaim = Some(Reclaim::Credited);
                effects.payout = Some(payout);
            }
            _ => return None,
        }
        Some(())
    }

    /// What a payout stopped for a human or undone changes: stopped, a controller's
    /// resolution, its undo.
    fn apply_payout_undo(
        &mut self,
        step: &PoolStep,
        lookup: &impl PoolLookup,
        at: Timestamp,
        lines: &mut PayoutLines,
        effects: &mut PoolEffects,
    ) -> Option<()> {
        match step {
            // back on its undo, its refund under the door's instant; or paid, from where a
            // reverted payout's USDC sits (reserved again)
            PoolStep::PayoutResolved {
                id,
                resolution,
                refund_at,
            } => {
                let mut payout = lookup.payout(*id)?;
                match resolution {
                    PayoutResolution::Refund => {
                        if payout.icp_released.0 {
                            let held = payout.icp_hold();
                            let icp = &mut self.pockets.icp;
                            icp.free = sat_sub(icp.free, held);
                            icp.reserved = sat_add(icp.reserved, held);
                            payout.icp_released = crate::pool::payout::Flag(false);
                        }
                        payout.status =
                            PayoutStatus::Undoing(crate::pool::payout::REFUNDED.to_string());
                        payout.undoing_at = refund_at.or(payout.undoing_at).or(Some(at));
                        lines.open.insert(*id);
                    }
                    PayoutResolution::MarkPaid => {
                        let amount = payout.amount.unwrap_or(TokenAmount::ZERO);
                        let base = &mut self.pockets.base;
                        base.reserved = sat_sub(base.reserved, amount);
                        base.in_flight = sat_add(base.in_flight, amount);
                        paid(self, lines, &mut payout, id);
                    }
                }
                effects.payout = Some(payout);
            }
            PoolStep::PayoutStopped { id, why } => {
                let mut payout = lookup.payout(*id)?;
                payout
                    .legs
                    .get_or_insert_with(crate::pool::PayoutProgress::default)
                    .failed = Some(why.clone());
                payout.status = PayoutStatus::Failed(why.clone());
                lines.open.remove(id);
                effects.payout = Some(payout);
            }
            PoolStep::PayoutUndoing { id, why } => {
                let mut payout = lookup.payout(*id)?;
                if payout.icp_released.0 {
                    let held = payout.icp_hold();
                    let icp = &mut self.pockets.icp;
                    icp.free = sat_sub(icp.free, held);
                    icp.reserved = sat_add(icp.reserved, held);
                    payout.icp_released = crate::pool::payout::Flag(false);
                }
                payout.status = PayoutStatus::Undoing(why.clone());
                payout.undoing_at = Some(at);
                effects.payout = Some(payout);
            }
            PoolStep::PayoutUndone { id, why, block } => {
                let mut payout = lookup.payout(*id)?;
                dequeue(lines, &mut payout, effects, at);
                if payout.base_reserved {
                    let hold = payout.base_hold();
                    let base = &mut self.pockets.base;
                    base.reserved = sat_sub(base.reserved, hold);
                    base.free = sat_add(base.free, hold);
                    payout.base_reserved = false;
                }
                self.owed = sat_sub(self.owed, payout.owed());
                // an ICP swap's ckUSDC leaves with its refund, its fee with it: from its hold,
                // or from the free ckUSDC once it was paid and released (an Eco refund)
                let icp = &mut self.pockets.icp;
                if payout.icp_released.0 {
                    icp.free = sat_sub(icp.free, payout.icp_hold());
                } else {
                    icp.reserved = sat_sub(icp.reserved, payout.icp_hold());
                }
                lines.fees_icp = sat_sub(lines.fees_icp, payout.icp_fee());
                if let Undo::Remint { owner, shares } = payout.undo {
                    self.total_shares = self
                        .total_shares
                        .checked_add(shares)
                        .unwrap_or(Shares::new(u128::MAX));
                    effects.shares = Some(credit_shares(lookup, &owner, shares));
                }
                lines.open.remove(id);
                payout.status = PayoutStatus::Undone(why.clone());
                payout.icp_block = *block;
                effects.payout = Some(payout);
            }
            _ => return None,
        }
        Some(())
    }

    /// What the steps of the transfers no deposit took change: booked, refunded, claimed by
    /// their terms.
    fn apply_orphan(
        &mut self,
        step: &PoolStep,
        lookup: &impl PoolLookup,
        lines: &mut PayoutLines,
        effects: &mut PoolEffects,
    ) -> Option<()> {
        match step {
            // out of the pool's unattributed line: to the Base books' (the state moves the
            // books), or on its way back to its payer, the fee kept on the Base fee line
            PoolStep::OrphanBooked { key } => {
                let mut orphan = lookup.orphan(key)?;
                self.unattributed = sat_sub(self.unattributed, orphan.amount);
                orphan.status = crate::pool::OrphanStatus::Booked;
                effects.orphan = Some((*key, orphan));
            }
            // off the unattributed line, for the deposit's own arrival line next; or terms
            // the pool does not take: held for a controller, with why
            PoolStep::TermsClaimed { credit, refused } => {
                let mut orphan = lookup.orphan(credit)?;
                match refused {
                    Some(why) => {
                        orphan.status = crate::pool::OrphanStatus::Held;
                        orphan.why = why.clone();
                    }
                    None => {
                        self.unattributed = sat_sub(self.unattributed, orphan.amount);
                        orphan.status = crate::pool::OrphanStatus::Claimed;
                    }
                }
                effects.orphan = Some((*credit, orphan));
            }
            PoolStep::OrphanRefundSigned { key, to, sent, .. } => {
                let mut orphan = lookup.orphan(key)?;
                self.unattributed = sat_sub(self.unattributed, orphan.amount);
                lines.fees_base = sat_add(lines.fees_base, sat_sub(orphan.amount, *sent));
                orphan.status = crate::pool::OrphanStatus::Refunding {
                    to: *to,
                    sent: *sent,
                };
                effects.orphan = Some((*key, orphan));
            }
            PoolStep::OrphanRefundClosed { key, success } => {
                let mut orphan = lookup.orphan(key)?;
                let crate::pool::OrphanStatus::Refunding { to, sent } = orphan.status else {
                    return None;
                };
                if *success {
                    orphan.status = crate::pool::OrphanStatus::Refunded { to, sent };
                } else {
                    // reverted: nothing left, held again
                    self.unattributed = sat_add(self.unattributed, orphan.amount);
                    lines.fees_base = sat_sub(lines.fees_base, sat_sub(orphan.amount, sent));
                    orphan.status = crate::pool::OrphanStatus::Held;
                }
                effects.orphan = Some((*key, orphan));
            }
            _ => return None,
        }
        Some(())
    }
}
