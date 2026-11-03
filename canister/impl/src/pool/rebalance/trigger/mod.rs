//! When the canister opens a move (spec 3.4): the ICP pocket's share leaves
//! `target_icp_bps ± band_bps` (the move takes it back to the target), or at once when a
//! payout waits on a pocket that cannot cover it (urgent: what the queue needs and the way
//! back to the target after it, at least what it needs, no least move). Never while a move in
//! the same direction is open, never above `max_move` or past `daily_cap` (a rolling 24 h
//! sum), never more than the source pocket holds behind its own queue; the caller never asks
//! while the pool is closed.
//!
//! The split is read with every open move counted where it is going: a move on its way is as
//! good as arrived for the share, so a drift is answered once.

#[cfg(test)]
mod tests;

use super::{cost, open_moves};
use crate::pool;
use crate::storage::config;
use crate::storage::events::read_state;
use types::pool::{Move, MoveDirection, PoolLedger, PoolStep, RebalanceSettings};
use types::{Timestamp, TokenAmount};

/// A move the books ask for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Want {
    pub direction: MoveDirection,
    pub amount: TokenAmount,
    pub urgent: bool,
}

fn signed(amount: TokenAmount) -> i128 {
    amount
        .try_into_u128()
        .and_then(|amount| i128::try_from(amount).ok())
        .unwrap_or(i128::MAX)
}

fn amount_of(value: i128) -> TokenAmount {
    u128::try_from(value.max(0)).map_or(TokenAmount::ZERO, TokenAmount::new)
}

/// The two pockets' USDC as the split reads them: each pocket's free and reserved lines (a
/// payout's USDC in flight has left the pool), every open move counted at its destination.
pub fn split(ledger: &PoolLedger, open: &[Move]) -> (i128, i128) {
    let held =
        |pocket: types::pool::Pocket| signed(pocket.free).saturating_add(signed(pocket.reserved));
    let mut icp = held(ledger.pockets.icp);
    let mut base = held(ledger.pockets.base);
    for item in open {
        let amount = signed(item.amount);
        let (from, to) = match item.direction {
            MoveDirection::BaseToIcp => (&mut base, &mut icp),
            MoveDirection::IcpToBase => (&mut icp, &mut base),
        };
        // reserved, it is still in its source's lines; in flight, it is in neither
        if !item.left {
            *from = from.saturating_sub(amount);
        }
        *to = to.saturating_add(amount);
    }
    (icp, base)
}

/// What the ICP pocket holds at the target share of `total`.
fn at_target(total: i128, settings: &RebalanceSettings) -> i128 {
    total.saturating_mul(i128::from(settings.target_icp_bps)) / 10_000
}

/// What a payout queue still needs of its pocket: everything it will hold, less the free
/// USDC there.
fn need(queued: TokenAmount, free: TokenAmount) -> i128 {
    signed(queued).saturating_sub(signed(free)).max(0)
}

/// The move the books ask for at `now`, if any. Pure: the caller reads the fold and the
/// clock, and checks the pool is open.
pub fn decide(
    ledger: &PoolLedger,
    open: &[Move],
    settings: &RebalanceSettings,
    now: Timestamp,
) -> Option<Want> {
    ledger.settings.as_ref()?;
    let busy = |direction| open.iter().any(|item| item.direction == direction);
    let (icp, base) = split(ledger, open);
    let total = icp.saturating_add(base);
    // the ICP queue (EVM to ICP swaps waiting for ckUSDC) and the Base queue (ICP to EVM
    // payouts and withdrawals waiting for USDC)
    let icp_need = need(ledger.queue.total(), ledger.pockets.icp.free);
    let base_need = need(ledger.lines().queued, ledger.pockets.base.free);
    let urgent = if icp_need > 0 && !busy(MoveDirection::BaseToIcp) {
        // after the queue is paid, the ICP pocket back at the target of what is left
        let after = total.saturating_sub(signed(ledger.queue.total()));
        let icp_after = icp.saturating_sub(signed(ledger.queue.total()));
        let amount = at_target(after, settings).saturating_sub(icp_after);
        Some((MoveDirection::BaseToIcp, amount.max(icp_need)))
    } else if base_need > 0 && !busy(MoveDirection::IcpToBase) {
        let after = total.saturating_sub(signed(ledger.lines().queued));
        let base_after = base.saturating_sub(signed(ledger.lines().queued));
        let target_base = after.saturating_sub(at_target(after, settings));
        let amount = target_base.saturating_sub(base_after);
        Some((MoveDirection::IcpToBase, amount.max(base_need)))
    } else {
        None
    };
    let (direction, wanted, urgent) = match urgent {
        Some((direction, amount)) => (direction, amount, true),
        None => {
            if total <= 0 {
                return None;
            }
            let share = icp.saturating_mul(10_000) / total;
            let target = i128::from(settings.target_icp_bps);
            let band = i128::from(settings.band_bps);
            let to_target = at_target(total, settings).saturating_sub(icp);
            if share < target - band {
                (MoveDirection::BaseToIcp, to_target, false)
            } else if share > target + band {
                (MoveDirection::IcpToBase, -to_target, false)
            } else {
                return None;
            }
        }
    };
    if busy(direction) {
        return None;
    }
    let available = match direction {
        MoveDirection::BaseToIcp => ledger.base_available(),
        MoveDirection::IcpToBase => ledger.icp_available(),
    };
    let day_left = settings
        .daily_cap
        .checked_sub(ledger.moves().opened_within_day(now))
        .unwrap_or(TokenAmount::ZERO);
    let amount = amount_of(wanted)
        .min(settings.max_move)
        .min(available)
        .min(day_left);
    if amount == TokenAmount::ZERO || (!urgent && amount < settings.min_move) {
        return None;
    }
    Some(Want {
        direction,
        amount,
        urgent,
    })
}

/// Opens the move the books ask for at `now`, if any, and answers its id: nothing while the
/// pool is closed, before a controller set the rebalancing's knobs, or with no pool chain. A
/// move to ICP that is not urgent takes proposals for `proposal_window` from now; a move to
/// Base takes them once its USDC lands on Ethereum.
pub fn open_if_needed(now: Timestamp) -> Option<u64> {
    if pool::pool_closed().is_some() {
        return None;
    }
    let config = config::get();
    let pool_chain = config.pool_chain?;
    let (want, settings, id) = read_state(|state| {
        let ledger = state.pool();
        let lines = ledger.moves();
        let settings = lines.settings?;
        let want = decide(&ledger, &open_moves(state), &settings, now)?;
        Some((want, settings, lines.next_id))
    })?;
    let window_ends = (want.direction == MoveDirection::BaseToIcp
        && !want.urgent
        && !settings.proposal_window.is_zero())
    .then(|| now.checked_add(settings.proposal_window))
    .flatten();
    let own_cost = cost::own_cost(
        &config,
        pool_chain,
        want.direction,
        want.amount,
        want.urgent,
        &settings,
    );
    let step = PoolStep::MoveOpened {
        id,
        direction: want.direction,
        amount: want.amount,
        urgent: want.urgent,
        opened_at: now,
        window_ends,
        own_cost,
    };
    match crate::storage::pool::record_at(step, now) {
        Ok(_) => {
            pool::log(&format!(
                "rebalance: move {id} opened, {:?} of {}{}",
                want.direction,
                want.amount,
                if want.urgent { ", urgent" } else { "" }
            ));
            Some(id)
        }
        Err(error) => {
            pool::log(&format!(
                "rebalance: the move the books ask for was refused: {error}"
            ));
            None
        }
    }
}
