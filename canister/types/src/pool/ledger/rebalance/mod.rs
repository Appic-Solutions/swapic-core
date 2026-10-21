//! The rebalancing's rules (P11c): what each move step must meet, read off the fold alone,
//! and what it moves. Each pocket move is refused in `admits` before `apply` could clamp it.
//!
//! - Opened: the amount `free -> reserved` in the source pocket, behind that pocket's queue.
//! - Leaving: Base to ICP at its first order's signature, ICP to Base at the minter's call:
//!   `reserved -> in_flight`. A revert of that first order, or the minter's refusal, puts it
//!   back.
//! - Arrived: `in_flight` out of the source, what arrived into the destination's `free` (on
//!   Base by its transfer's credit key, once), the difference to the moves' cost line.
//! - Refunded (Base to ICP by Eco, unfilled): back to `reserved`, routed again.
//! - Lost: a controller's write-off of what is reserved or in flight.

#[cfg(test)]
mod tests;

use super::{sat_add, sat_sub, PoolEffects, PoolLedger, PoolLookup, PoolStepError};
use crate::numeric::{Timestamp, TokenAmount};
use crate::pool::rebalance::{
    Landing, Move, MoveDirection, MoveRoute, MoveStatus, Proposal, RebalanceLines, Sweep, DAY,
};
use crate::pool::{Pocket, Pockets, PoolStep, UnseenArrival};

fn status_of(item: &Move) -> String {
    format!("{:?}", item.status)
}

/// The pocket a move leaves.
fn source(pockets: &mut Pockets, direction: MoveDirection) -> &mut Pocket {
    match direction {
        MoveDirection::BaseToIcp => &mut pockets.base,
        MoveDirection::IcpToBase => &mut pockets.icp,
    }
}

/// The pocket a move arrives in.
fn destination(pockets: &mut Pockets, direction: MoveDirection) -> &mut Pocket {
    match direction {
        MoveDirection::BaseToIcp => &mut pockets.icp,
        MoveDirection::IcpToBase => &mut pockets.base,
    }
}

impl PoolLedger {
    /// The rebalancing's lines, empty before its first step.
    pub fn moves(&self) -> RebalanceLines {
        self.rebalance.clone().unwrap_or_default()
    }

    pub(super) fn admits_rebalance(
        &self,
        step: &PoolStep,
        lookup: &impl PoolLookup,
    ) -> Result<(), PoolStepError> {
        let lines = self.moves();
        let move_of = |id: u64| lookup.pool_move(id).ok_or(PoolStepError::UnknownMove(id));
        let not_from = |step: &'static str, item: &Move| PoolStepError::MoveNotFrom {
            step,
            id: item.id,
            status: status_of(item),
        };
        match step {
            PoolStep::MoveSettingsSet { settings } => Ok(settings.validate()?),
            PoolStep::MoveOpened {
                id,
                direction,
                amount,
                urgent,
                opened_at,
                ..
            } => {
                self.settings()?;
                let settings = lines.settings.ok_or(PoolStepError::NoRebalanceSettings)?;
                if *id != lines.next_id || lookup.pool_move(*id).is_some() {
                    return Err(PoolStepError::MoveId {
                        expected: lines.next_id,
                    });
                }
                if *amount == TokenAmount::ZERO {
                    return Err(PoolStepError::Zero("amount"));
                }
                if *amount > settings.max_move {
                    return Err(PoolStepError::Terms("a move carries at most max_move"));
                }
                if !urgent && *amount < settings.min_move {
                    return Err(PoolStepError::Terms(
                        "a move that no payout waits for carries at least min_move",
                    ));
                }
                let same_way = lines
                    .open
                    .iter()
                    .filter_map(|open| lookup.pool_move(*open))
                    .any(|open| open.direction == *direction);
                if same_way {
                    return Err(PoolStepError::Terms(
                        "a move in the same direction is in flight",
                    ));
                }
                let within_day = sat_add(lines.opened_within_day(*opened_at), *amount);
                if within_day > settings.daily_cap {
                    return Err(PoolStepError::Terms("the moves of a day pass the daily cap"));
                }
                match direction {
                    MoveDirection::BaseToIcp if *amount > self.base_available() => {
                        Err(PoolStepError::BaseShort {
                            free: self.base_available(),
                            need: *amount,
                        })
                    }
                    MoveDirection::IcpToBase if *amount > self.icp_available() => {
                        Err(PoolStepError::PocketShort {
                            free: self.icp_available(),
                            need: *amount,
                        })
                    }
                    _ => Ok(()),
                }
            }
            PoolStep::MoveProposed { id, fee, .. } => {
                let item = move_of(*id)?;
                if !item.proposable() {
                    return Err(not_from("MoveProposed", &item));
                }
                if item.proposal.as_ref().is_some_and(|taken| *fee >= taken.fee) {
                    return Err(PoolStepError::Terms(
                        "a proposal is taken only when cheaper than the one taken",
                    ));
                }
                let settings = lines.settings.ok_or(PoolStepError::NoRebalanceSettings)?;
                if *fee > settings.proposal_fee_cap(item.amount) {
                    return Err(PoolStepError::Terms("the proposal's fee is above the cap"));
                }
                Ok(())
            }
            PoolStep::MoveRouted {
                id, route, order, ..
            } => {
                let item = move_of(*id)?;
                if !item.routable() {
                    return Err(not_from("MoveRouted", &item));
                }
                // a proposal routes the move only as it was taken
                if let MoveRoute::Proposed { fee } = route {
                    let taken = item.proposal.as_ref().ok_or(PoolStepError::Mismatch)?;
                    if taken.fee != *fee || taken.order.order_id != order.order_id {
                        return Err(PoolStepError::Mismatch);
                    }
                }
                Ok(())
            }
            PoolStep::MoveSigned { id, .. } => {
                let item = move_of(*id)?;
                let deposit = item.direction == MoveDirection::BaseToIcp
                    && item.status == MoveStatus::Landed
                    && item.route.is_some_and(|route| route.is_eco());
                if item.stuck.is_some() || !(item.status == MoveStatus::Routed || deposit) {
                    return Err(not_from("MoveSigned", &item));
                }
                // a Base to ICP move's first order leaves the Base pocket
                if !item.left && item.amount > self.pockets.base.reserved {
                    return Err(PoolStepError::Overflow);
                }
                Ok(())
            }
            PoolStep::MoveBurned { id, .. } | PoolStep::MoveReverted { id } => {
                let item = move_of(*id)?;
                if !matches!(item.status, MoveStatus::Signed(_)) {
                    return Err(not_from(step.name(), &item));
                }
                Ok(())
            }
            PoolStep::MoveWithdrawing { id, .. } => {
                let item = move_of(*id)?;
                if item.direction != MoveDirection::IcpToBase
                    || item.status != MoveStatus::Open
                    || item.stuck.is_some()
                    || item.left
                {
                    return Err(not_from("MoveWithdrawing", &item));
                }
                if item.amount > self.pockets.icp.reserved {
                    return Err(PoolStepError::Overflow);
                }
                Ok(())
            }
            PoolStep::MoveWithdrawn { id, fee, .. } | PoolStep::MoveRefused { id, fee, .. } => {
                let item = move_of(*id)?;
                if item.direction != MoveDirection::IcpToBase
                    || item.status != MoveStatus::Withdrawing
                {
                    return Err(not_from(step.name(), &item));
                }
                if *fee > self.pockets.icp.free {
                    return Err(PoolStepError::Overflow);
                }
                Ok(())
            }
            PoolStep::MoveLanded { id, amount, .. } => {
                let item = move_of(*id)?;
                let landing = match item.direction {
                    MoveDirection::IcpToBase => item.status == MoveStatus::Withdrawn,
                    MoveDirection::BaseToIcp => {
                        item.status == MoveStatus::Burned
                            && item.route.is_some_and(|route| route.is_eco())
                            && item.landed.is_none()
                    }
                };
                if !landing {
                    return Err(not_from("MoveLanded", &item));
                }
                if *amount == TokenAmount::ZERO {
                    return Err(PoolStepError::Zero("amount"));
                }
                Ok(())
            }
            PoolStep::MoveSweeping {
                id,
                amount,
                credited,
                ..
            } => {
                let item = move_of(*id)?;
                // after its own burn, or after its ck deposit once an Eco fill landed
                let swept = item.direction == MoveDirection::BaseToIcp
                    && item.status == MoveStatus::Burned
                    && match item.route {
                        Some(MoveRoute::Own { .. }) => true,
                        Some(MoveRoute::Proposed { .. }) => item.landed.is_some(),
                        None => false,
                    };
                if !swept || item.stuck.is_some() {
                    return Err(not_from("MoveSweeping", &item));
                }
                if *credited > *amount || *credited > item.creditable() {
                    return Err(PoolStepError::Terms(
                        "a move is credited at most what it may carry, and what is swept",
                    ));
                }
                Ok(())
            }
            PoolStep::MoveArrived {
                id,
                credited,
                credit,
                ..
            } => {
                let item = move_of(*id)?;
                match item.direction {
                    MoveDirection::BaseToIcp => {
                        let swept = item.sweep.map(|sweep| sweep.credited);
                        if item.status != MoveStatus::Sweeping || swept != Some(*credited) {
                            return Err(not_from("MoveArrived", &item));
                        }
                        if credit.is_some() {
                            return Err(PoolStepError::Mismatch);
                        }
                    }
                    MoveDirection::IcpToBase => {
                        if item.status != MoveStatus::Burned {
                            return Err(not_from("MoveArrived", &item));
                        }
                        let key = credit.ok_or(PoolStepError::Mismatch)?;
                        if lookup.is_credited(&key) {
                            return Err(PoolStepError::AlreadyCredited(key));
                        }
                        if *credited > item.amount {
                            return Err(PoolStepError::Terms(
                                "a move is credited at most its amount",
                            ));
                        }
                    }
                }
                let mut pockets = self.pockets;
                if !item.left || item.amount > source(&mut pockets, item.direction).in_flight {
                    return Err(PoolStepError::Overflow);
                }
                Ok(())
            }
            PoolStep::MoveStuck { id, .. } => {
                let item = move_of(*id)?;
                if item.status.is_final() || item.stuck.is_some() {
                    return Err(not_from("MoveStuck", &item));
                }
                Ok(())
            }
            PoolStep::MoveRetried { id } => {
                let item = move_of(*id)?;
                if item.status.is_final() || item.stuck.is_none() {
                    return Err(not_from("MoveRetried", &item));
                }
                Ok(())
            }
            PoolStep::MoveLost { id, .. } => {
                let item = move_of(*id)?;
                if item.status.is_final() {
                    return Err(not_from("MoveLost", &item));
                }
                let mut pockets = self.pockets;
                let pocket = source(&mut pockets, item.direction);
                let held = if item.left {
                    pocket.in_flight
                } else {
                    pocket.reserved
                };
                if item.amount > held {
                    return Err(PoolStepError::Overflow);
                }
                Ok(())
            }
            PoolStep::MoveRefunded {
                id, credit, amount, ..
            } => {
                let item = move_of(*id)?;
                let refundable = item.direction == MoveDirection::BaseToIcp
                    && item.status == MoveStatus::Burned
                    && item.route.is_some_and(|route| route.is_eco())
                    && item.landed.is_none()
                    && item.intent.is_some();
                if !refundable {
                    return Err(not_from("MoveRefunded", &item));
                }
                if lookup.is_credited(credit) {
                    return Err(PoolStepError::AlreadyCredited(*credit));
                }
                if *amount != item.amount {
                    return Err(PoolStepError::Terms(
                        "an Eco refund gives back exactly the move's amount",
                    ));
                }
                if item.amount > self.pockets.base.in_flight {
                    return Err(PoolStepError::Overflow);
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// What a move step changes, None for a step that is not one.
    pub(super) fn apply_rebalance(
        self,
        step: &PoolStep,
        lookup: &impl PoolLookup,
        at: Timestamp,
        index: u64,
    ) -> Option<PoolEffects> {
        let mut ledger = self;
        let mut lines = ledger.moves();
        let mut credited = None;
        let moved = match step {
            PoolStep::MoveSettingsSet { settings } => {
                lines.settings = Some(*settings);
                None
            }
            PoolStep::MoveOpened {
                id,
                direction,
                amount,
                urgent,
                opened_at,
                window_ends,
                own_cost,
            } => {
                lines.next_id = id.saturating_add(1);
                lines.open.insert(*id);
                let since = opened_at.checked_sub(DAY).unwrap_or(Timestamp::from_nanos(0));
                lines.recent.retain(|(opened, _)| *opened > since);
                lines.recent.push((*opened_at, *amount));
                let pocket = source(&mut ledger.pockets, *direction);
                pocket.free = sat_sub(pocket.free, *amount);
                pocket.reserved = sat_add(pocket.reserved, *amount);
                Some(Move::opened(
                    *id,
                    *direction,
                    *amount,
                    *urgent,
                    *opened_at,
                    *window_ends,
                    *own_cost,
                ))
            }
            PoolStep::MoveProposed { id, order, fee } => {
                let mut item = lookup.pool_move(*id)?;
                item.proposal = Some(Proposal {
                    order: order.clone(),
                    fee: *fee,
                });
                Some(item)
            }
            PoolStep::MoveRouted {
                id,
                route,
                order,
                scan_from,
            } => {
                let mut item = lookup.pool_move(*id)?;
                item.route = Some(*route);
                item.order = Some(order.clone());
                item.scan_from = *scan_from;
                item.status = MoveStatus::Routed;
                item.since = at;
                Some(item)
            }
            PoolStep::MoveSigned { id, tx_hash, .. } => {
                let mut item = lookup.pool_move(*id)?;
                // a Base to ICP move leaves the Base pocket with its first order
                if !item.left {
                    let base = &mut ledger.pockets.base;
                    base.reserved = sat_sub(base.reserved, item.amount);
                    base.in_flight = sat_add(base.in_flight, item.amount);
                    item.left = true;
                }
                item.status = MoveStatus::Signed(index);
                item.tx_hash = Some(*tx_hash);
                item.since = at;
                Some(item)
            }
            PoolStep::MoveBurned { id, intent, .. } => {
                let mut item = lookup.pool_move(*id)?;
                if intent.is_some() {
                    item.intent = *intent;
                }
                item.status = MoveStatus::Burned;
                item.since = at;
                Some(item)
            }
            PoolStep::MoveReverted { id } => {
                let mut item = lookup.pool_move(*id)?;
                match (item.direction, item.landed.is_some()) {
                    // its first order: nothing left the Base pocket
                    (MoveDirection::BaseToIcp, false) => {
                        let base = &mut ledger.pockets.base;
                        base.in_flight = sat_sub(base.in_flight, item.amount);
                        base.reserved = sat_add(base.reserved, item.amount);
                        item.left = false;
                        item.status = MoveStatus::Routed;
                    }
                    // its ck deposit: the USDC is still at the canister's Ethereum address, and a
                    // deposit carries no id a retry could be refused by: a controller decides
                    (MoveDirection::BaseToIcp, true) => {
                        item.status = MoveStatus::Landed;
                        item.stuck = Some(
                            "its ck deposit reverted: a retry deposits again, once a reorg could \
                             not have run the first"
                                .to_string(),
                        );
                    }
                    (MoveDirection::IcpToBase, _) => item.status = MoveStatus::Routed,
                }
                item.reverts = Some(item.reverts.unwrap_or(0).saturating_add(1));
                item.since = at;
                Some(item)
            }
            PoolStep::MoveWithdrawing { id, created_at } => {
                let mut item = lookup.pool_move(*id)?;
                let icp = &mut ledger.pockets.icp;
                icp.reserved = sat_sub(icp.reserved, item.amount);
                icp.in_flight = sat_add(icp.in_flight, item.amount);
                item.left = true;
                item.minter_at = Some(*created_at);
                item.status = MoveStatus::Withdrawing;
                item.since = at;
                Some(item)
            }
            PoolStep::MoveWithdrawn {
                id,
                withdrawal_id,
                fee,
            } => {
                let mut item = lookup.pool_move(*id)?;
                ledger.pockets.icp.free = sat_sub(ledger.pockets.icp.free, *fee);
                lines.cost = sat_add(lines.cost, *fee);
                item.withdrawal = Some(*withdrawal_id);
                item.status = MoveStatus::Withdrawn;
                item.since = at;
                Some(item)
            }
            PoolStep::MoveRefused { id, fee, .. } => {
                let mut item = lookup.pool_move(*id)?;
                let icp = &mut ledger.pockets.icp;
                icp.in_flight = sat_sub(icp.in_flight, item.amount);
                icp.reserved = sat_add(icp.reserved, item.amount);
                icp.free = sat_sub(icp.free, *fee);
                lines.cost = sat_add(lines.cost, *fee);
                item.left = false;
                item.minter_at = None;
                item.status = MoveStatus::Open;
                item.since = at;
                Some(item)
            }
            PoolStep::MoveLanded {
                id,
                tx_hash,
                block,
                amount,
                window_ends,
            } => {
                let mut item = lookup.pool_move(*id)?;
                item.landed = Some(Landing {
                    tx: *tx_hash,
                    block: *block,
                    amount: *amount,
                });
                if item.direction == MoveDirection::IcpToBase {
                    item.window_ends = *window_ends;
                }
                item.status = MoveStatus::Landed;
                item.since = at;
                Some(item)
            }
            PoolStep::MoveSweeping {
                id,
                created_at,
                amount,
                credited,
            } => {
                let mut item = lookup.pool_move(*id)?;
                item.sweep = Some(Sweep {
                    created_at: *created_at,
                    amount: *amount,
                    credited: *credited,
                });
                item.status = MoveStatus::Sweeping;
                item.since = at;
                Some(item)
            }
            PoolStep::MoveArrived {
                id,
                credited: amount,
                credit,
                block,
                ..
            } => {
                let mut item = lookup.pool_move(*id)?;
                let from = source(&mut ledger.pockets, item.direction);
                from.in_flight = sat_sub(from.in_flight, item.amount);
                let to = destination(&mut ledger.pockets, item.direction);
                to.free = sat_add(to.free, *amount);
                lines.cost = sat_add(lines.cost, sat_sub(item.amount, *amount));
                lines.open.remove(id);
                // a Base arrival from a block no balance check has read at yet
                if let (Some(key), Some(block)) = (credit, block) {
                    ledger.unseen.push(UnseenArrival {
                        chain: key.chain,
                        block: *block,
                        amount: *amount,
                    });
                }
                credited = *credit;
                item.arrived = Some(*amount);
                item.status = MoveStatus::Arrived;
                item.stuck = None;
                item.since = at;
                Some(item)
            }
            PoolStep::MoveStuck { id, why } => {
                let mut item = lookup.pool_move(*id)?;
                item.stuck = Some(why.clone());
                Some(item)
            }
            // on from where it stood; a minter's call a controller found never ran is asked again
            PoolStep::MoveRetried { id } => {
                let mut item = lookup.pool_move(*id)?;
                if item.status == MoveStatus::Withdrawing {
                    let icp = &mut ledger.pockets.icp;
                    icp.in_flight = sat_sub(icp.in_flight, item.amount);
                    icp.reserved = sat_add(icp.reserved, item.amount);
                    item.left = false;
                    item.minter_at = None;
                    item.status = MoveStatus::Open;
                }
                item.stuck = None;
                item.since = at;
                Some(item)
            }
            PoolStep::MoveLost { id, why } => {
                let mut item = lookup.pool_move(*id)?;
                let pocket = source(&mut ledger.pockets, item.direction);
                if item.left {
                    pocket.in_flight = sat_sub(pocket.in_flight, item.amount);
                } else {
                    pocket.reserved = sat_sub(pocket.reserved, item.amount);
                }
                lines.lost = sat_add(lines.lost, item.amount);
                lines.open.remove(id);
                item.status = MoveStatus::Lost(why.clone());
                item.stuck = None;
                item.since = at;
                Some(item)
            }
            // routed again, at once, on the canister's own route
            PoolStep::MoveRefunded {
                id,
                credit,
                amount,
                block,
            } => {
                let mut item = lookup.pool_move(*id)?;
                let base = &mut ledger.pockets.base;
                base.in_flight = sat_sub(base.in_flight, item.amount);
                base.reserved = sat_add(base.reserved, item.amount);
                ledger.unseen.push(UnseenArrival {
                    chain: credit.chain,
                    block: *block,
                    amount: *amount,
                });
                credited = Some(*credit);
                item.left = false;
                item.route = None;
                item.order = None;
                item.proposal = None;
                item.intent = None;
                item.tx_hash = None;
                item.scan_from = None;
                item.window_ends = None;
                item.status = MoveStatus::Open;
                item.since = at;
                Some(item)
            }
            _ => return None,
        };
        ledger.rebalance = Some(lines);
        Some(PoolEffects {
            ledger,
            deposit: None,
            shares: None,
            credited,
            payout: None,
            queued: None,
            dequeued: None,
            nonce: None,
            intent: None,
            orphan: None,
            moved,
        })
    }
}
