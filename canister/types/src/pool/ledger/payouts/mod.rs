//! The Base payouts' rules (P11b): what each payout step must meet, read off the fold alone,
//! and what it moves. Each pocket move is refused in `admits` before `apply` could clamp it.
//!
//! - Opened: an ICP swap with nothing booked yet; a withdrawal burns its shares and spends its
//!   nonce.
//! - AmountKnown: `owed` gains the payout (and a withdrawal's network fee, held on Base with
//!   it); an ICP swap's ckUSDC is held in `icp.reserved` but its network fee, which goes to
//!   the ICP fee line.
//! - Queued / Waiting: first in first out; Waiting reserves the Base hold (`free -> reserved`).
//! - Signed: the order's amount `reserved -> in_flight`. Reverted: back to `reserved`.
//! - Paid: `in_flight` out, a withdrawal's fee to the Base fee line, an ICP swap's ckUSDC
//!   freed to the pool, `owed` down.
//! - Undone: everything released, an ICP swap's ckUSDC out with its refund, a withdrawal's
//!   shares minted back.

#[cfg(test)]
mod tests;

use super::{credit_shares, sat_add, sat_sub, PoolEffects, PoolLedger, PoolLookup, PoolStepError};
use crate::numeric::{Timestamp, TokenAmount};
use crate::pool::payout::{EvmPayout, NonceUse, PayoutKind, PayoutStatus, Undo};
use crate::pool::{PoolAccount, PoolStep, ShareLine, Shares};

fn status_of(payout: &EvmPayout) -> String {
    format!("{:?}", payout.status)
}

impl PoolLedger {
    /// What the Base pocket's free USDC covers behind the queue.
    pub fn base_available(&self) -> TokenAmount {
        sat_sub(self.pockets.base.free, self.lines().queued)
    }

    pub(super) fn admits_payout(
        &self,
        step: &PoolStep,
        lookup: &impl PoolLookup,
    ) -> Result<(), PoolStepError> {
        let lines = self.lines();
        let payout_of = |id: u64| lookup.payout(id).ok_or(PoolStepError::UnknownPayout(id));
        let not_from = |step: &'static str, payout: &EvmPayout| PoolStepError::PayoutNotFrom {
            step,
            id: payout.id,
            status: status_of(payout),
        };
        match step {
            PoolStep::PayoutSettingsSet { settings } => Ok(settings.validate()?),
            PoolStep::PayoutOpened {
                id,
                kind,
                terms,
                undo,
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
                    }
                    PayoutKind::Withdrawal(withdrawal) => {
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
                let payout = payout_of(*id)?;
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
                        let expected = received
                            .checked_sub(swap.premium)
                            .and_then(|left| left.checked_sub(swap.network_fee));
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
            PoolStep::PayoutQueued { id } => {
                let payout = payout_of(*id)?;
                if payout.status != PayoutStatus::Known {
                    return Err(not_from("PayoutQueued", &payout));
                }
                Ok(())
            }
            // first in first out: a new payout waits only behind an empty queue, a queued one
            // only from the head; and only once the free USDC covers its whole hold
            PoolStep::PayoutWaiting { id } => {
                let payout = payout_of(*id)?;
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
            // a payout waiting is signed once; a retry after a revert signs the order it pinned
            PoolStep::PayoutSigned { id, order, .. } => {
                let payout = payout_of(*id)?;
                if payout.status != PayoutStatus::Waiting || !payout.base_reserved {
                    return Err(not_from("PayoutSigned", &payout));
                }
                if let Some(pinned) = &payout.order {
                    if pinned.order_id != order.order_id {
                        return Err(PoolStepError::Terms(
                            "a payout retried after a revert signs the order it pinned",
                        ));
                    }
                }
                let amount = payout.amount.ok_or(PoolStepError::Mismatch)?;
                if amount > self.pockets.base.reserved {
                    return Err(PoolStepError::Overflow);
                }
                Ok(())
            }
            PoolStep::PayoutPaid { id, .. } | PoolStep::PayoutReverted { id } => {
                let payout = payout_of(*id)?;
                if !matches!(payout.status, PayoutStatus::Signed(_)) {
                    return Err(not_from(step.name(), &payout));
                }
                let amount = payout.amount.ok_or(PoolStepError::Mismatch)?;
                if amount > self.pockets.base.in_flight {
                    return Err(PoolStepError::Overflow);
                }
                Ok(())
            }
            // the pull of an ICP swap opened and not pulled yet
            PoolStep::PayoutPulled { id, attempt, .. } => {
                let payout = payout_of(*id)?;
                if payout.status != PayoutStatus::Opened || !payout.is_icp_swap() {
                    return Err(not_from("PayoutPulled", &payout));
                }
                if attempt.is_some_and(|attempt| !attempt.path.swaps()) {
                    return Err(PoolStepError::Terms("a pull's legs swap on ICPSwap"));
                }
                Ok(())
            }
            // what happened to its money is not known: only before it waits for an order
            PoolStep::PayoutStopped { id, .. } => {
                let payout = payout_of(*id)?;
                if !matches!(
                    payout.status,
                    PayoutStatus::Opened | PayoutStatus::Swapping | PayoutStatus::Undoing(_)
                ) {
                    return Err(not_from("PayoutStopped", &payout));
                }
                Ok(())
            }
            PoolStep::PayoutRelisted { id } => {
                let payout = payout_of(*id)?;
                if payout.status != PayoutStatus::Waiting {
                    return Err(not_from("PayoutRelisted", &payout));
                }
                Ok(())
            }
            // the refund of the intent the paid payout published, all of its reward
            PoolStep::PayoutRefunded {
                id, credit, amount, ..
            } => {
                let payout = payout_of(*id)?;
                if payout.status != PayoutStatus::Paid || payout.intent.is_none() {
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
                if fee > lines.fees_base {
                    return Err(PoolStepError::Overflow);
                }
                Ok(())
            }
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
            PoolStep::PayoutUndoing { id, .. } => {
                let payout = payout_of(*id)?;
                if !matches!(
                    payout.status,
                    PayoutStatus::Opened
                        | PayoutStatus::Swapping
                        | PayoutStatus::Known
                        | PayoutStatus::Queued
                        | PayoutStatus::Waiting
                ) {
                    return Err(not_from("PayoutUndoing", &payout));
                }
                // an ICP swap paid once and reopened by an Eco refund: its refund is held back
                // out of the free ckUSDC before it is asked, never after
                if payout.icp_released.0 && payout.icp_hold() > self.pockets.icp.free {
                    return Err(PoolStepError::PocketShort {
                        free: self.pockets.icp.free,
                        need: payout.icp_hold(),
                    });
                }
                Ok(())
            }
            // never once signed: a signed payout waits for its transaction's end
            PoolStep::PayoutUndone { id, .. } => {
                let payout = payout_of(*id)?;
                if !matches!(
                    payout.status,
                    PayoutStatus::Opened
                        | PayoutStatus::Swapping
                        | PayoutStatus::Known
                        | PayoutStatus::Queued
                        | PayoutStatus::Waiting
                        | PayoutStatus::Undoing(_)
                ) {
                    return Err(not_from("PayoutUndone", &payout));
                }
                if let Undo::Remint { shares, .. } = payout.undo {
                    self.total_shares
                        .checked_add(shares)
                        .ok_or(PoolStepError::Overflow)?;
                }
                // an ICP swap paid once, then refunded by Eco: its ckUSDC is the free one
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
        let mut effects = PoolEffects {
            ledger: PoolLedger::default(),
            deposit: None,
            shares: None,
            credited: None,
            payout: None,
            queued: None,
            dequeued: None,
            nonce: None,
            intent: None,
            orphan: None,
            moved: None,
        };
        // takes a payout off the queue, with its hold
        let dequeue = |lines: &mut crate::pool::PayoutLines,
                       payout: &mut EvmPayout,
                       effects: &mut PoolEffects| {
            if let Some(seq) = payout.queued_at.take() {
                effects.dequeued = Some(seq);
                lines.queue_len = lines.queue_len.saturating_sub(1);
                lines.queued = sat_sub(lines.queued, payout.base_hold());
                lines.head_since = (lines.queue_len > 0).then_some(at);
            }
        };
        match step {
            PoolStep::PayoutSettingsSet { settings } => lines.settings = Some(*settings),
            PoolStep::PayoutOpened {
                id,
                kind,
                terms,
                undo,
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
                    ledger.total_shares = ledger
                        .total_shares
                        .checked_sub(withdrawal.burned)
                        .unwrap_or(Shares::ZERO);
                    effects.nonce = Some(NonceUse {
                        staker: withdrawal.staker,
                        nonce: withdrawal.nonce,
                    });
                }
                effects.payout = Some(EvmPayout::new(*id, **kind, *terms, *undo, at));
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
                        let icp = &mut ledger.pockets.icp;
                        icp.reserved = sat_add(icp.reserved, payout.icp_hold());
                        lines.fees_icp = sat_add(lines.fees_icp, payout.icp_fee());
                    }
                    PayoutKind::Withdrawal(withdrawal) => {
                        payout.base_fee = Some(withdrawal.network_fee);
                    }
                }
                ledger.owed = sat_add(ledger.owed, payout.owed());
                effects.payout = Some(payout);
            }
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
                dequeue(&mut lines, &mut payout, &mut effects);
                let hold = payout.base_hold();
                let base = &mut ledger.pockets.base;
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
                let base = &mut ledger.pockets.base;
                base.reserved = sat_sub(base.reserved, amount);
                base.in_flight = sat_add(base.in_flight, amount);
                payout.status = PayoutStatus::Signed(index);
                payout.order = Some(order.clone());
                payout.tx_hash = Some(*tx_hash);
                effects.payout = Some(payout);
            }
            PoolStep::PayoutPaid {
                id,
                tx_hash,
                block,
                intent,
            } => {
                let mut payout = lookup.payout(*id)?;
                let amount = payout.amount.unwrap_or(TokenAmount::ZERO);
                let fee = payout.base_fee.unwrap_or(TokenAmount::ZERO);
                let base = &mut ledger.pockets.base;
                base.in_flight = sat_sub(base.in_flight, amount);
                base.reserved = sat_sub(base.reserved, fee);
                lines.fees_base = sat_add(lines.fees_base, fee);
                ledger.owed = sat_sub(ledger.owed, payout.owed());
                // the swap is done: its ckUSDC is the pool's now
                let held = payout.icp_hold();
                let icp = &mut ledger.pockets.icp;
                icp.reserved = sat_sub(icp.reserved, held);
                icp.free = sat_add(icp.free, held);
                lines.open.remove(id);
                payout.status = PayoutStatus::Paid;
                payout.base_reserved = false;
                payout.icp_released =
                    crate::pool::payout::Flag(payout.is_icp_swap() && payout.received.is_some());
                payout.tx_hash = Some(*tx_hash);
                payout.paid_block = Some(*block);
                payout.intent = *intent;
                effects.intent = intent.map(|intent| (intent, *id));
                effects.payout = Some(payout);
            }
            PoolStep::PayoutReverted { id } => {
                let mut payout = lookup.payout(*id)?;
                let amount = payout.amount.unwrap_or(TokenAmount::ZERO);
                let base = &mut ledger.pockets.base;
                base.in_flight = sat_sub(base.in_flight, amount);
                base.reserved = sat_add(base.reserved, amount);
                payout.status = PayoutStatus::Waiting;
                payout.waiting_since = Some(at);
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
            PoolStep::PayoutRelisted { id } => {
                let mut payout = lookup.payout(*id)?;
                payout.waiting_since = Some(at);
                effects.payout = Some(payout);
            }
            // the reverse of its paid line: the USDC back reserved for it, its fee back from
            // the fee line, owed again, and waiting for a new order (its intent can no longer
            // be filled, so its pin is cleared)
            PoolStep::PayoutRefunded {
                id,
                credit,
                amount,
                block,
            } => {
                let mut payout = lookup.payout(*id)?;
                let fee = payout.base_fee.unwrap_or(TokenAmount::ZERO);
                lines.fees_base = sat_sub(lines.fees_base, fee);
                let base = &mut ledger.pockets.base;
                base.reserved = sat_add(base.reserved, sat_add(*amount, fee));
                ledger.owed = sat_add(ledger.owed, payout.owed());
                ledger.unseen.push(crate::pool::UnseenArrival {
                    chain: credit.chain,
                    block: *block,
                    amount: *amount,
                });
                effects.credited = Some(*credit);
                lines.open.insert(*id);
                payout.status = PayoutStatus::Waiting;
                payout.base_reserved = true;
                payout.order = None;
                payout.tx_hash = None;
                payout.waiting_since = Some(at);
                effects.payout = Some(payout);
            }
            // out of the pool's unattributed line: to the Base books' (the state moves the
            // books), or on its way back to its payer, the fee kept on the Base fee line
            PoolStep::OrphanBooked { key } => {
                let mut orphan = lookup.orphan(key)?;
                ledger.unattributed = sat_sub(ledger.unattributed, orphan.amount);
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
                        ledger.unattributed = sat_sub(ledger.unattributed, orphan.amount);
                        orphan.status = crate::pool::OrphanStatus::Claimed;
                    }
                }
                effects.orphan = Some((*credit, orphan));
            }
            PoolStep::OrphanRefundSigned { key, to, sent, .. } => {
                let mut orphan = lookup.orphan(key)?;
                ledger.unattributed = sat_sub(ledger.unattributed, orphan.amount);
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
                    ledger.unattributed = sat_add(ledger.unattributed, orphan.amount);
                    lines.fees_base = sat_sub(lines.fees_base, sat_sub(orphan.amount, sent));
                    orphan.status = crate::pool::OrphanStatus::Held;
                }
                effects.orphan = Some((*key, orphan));
            }
            PoolStep::PayoutUndoing { id, why } => {
                let mut payout = lookup.payout(*id)?;
                if payout.icp_released.0 {
                    let held = payout.icp_hold();
                    let icp = &mut ledger.pockets.icp;
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
                dequeue(&mut lines, &mut payout, &mut effects);
                if payout.base_reserved {
                    let hold = payout.base_hold();
                    let base = &mut ledger.pockets.base;
                    base.reserved = sat_sub(base.reserved, hold);
                    base.free = sat_add(base.free, hold);
                    payout.base_reserved = false;
                }
                ledger.owed = sat_sub(ledger.owed, payout.owed());
                // an ICP swap's ckUSDC leaves with its refund, its fee with it: from its hold,
                // or from the free ckUSDC once it was paid and released (an Eco refund)
                let icp = &mut ledger.pockets.icp;
                if payout.icp_released.0 {
                    icp.free = sat_sub(icp.free, payout.icp_hold());
                } else {
                    icp.reserved = sat_sub(icp.reserved, payout.icp_hold());
                }
                lines.fees_icp = sat_sub(lines.fees_icp, payout.icp_fee());
                if let Undo::Remint { owner, shares } = payout.undo {
                    ledger.total_shares = ledger
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
        ledger.payouts = Some(lines);
        effects.ledger = ledger;
        Some(effects)
    }
}
