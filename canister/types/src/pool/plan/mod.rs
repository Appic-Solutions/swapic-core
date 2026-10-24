//! The payout plan's arithmetic (2026-10-23, the final review's M-01): an ICP user's swap is
//! opened with the quoter's ready-made order for the payout it expects, `planned` (the
//! order's `amountIn`), and one slippage. Half of the slippage guards the sale on ICP; the
//! premium and the network fee come from what the sale really made; the payout is what is
//! left, and the plan fits it when the two are within 5% of each other.
//!
//! Every function is pure and checked: `None` is an amount no swap may take, refused at the
//! opening and undone after it. Rounding is always toward the pool.

#[cfg(test)]
mod tests;

use crate::numeric::TokenAmount;
use crate::pool::{premium_share, PoolSettings};

/// The most slippage a planned swap may ask, in bps.
pub const MAX_SLIPPAGE_BPS: u16 = 500;

/// How far the payout may be from the plan's amount for the plan to fit it, in bps of the
/// plan's amount, either way.
pub const WINDOW_BPS: u32 = 500;

const BPS: u32 = 10_000;

/// The ckUSDC a sale must make for its payout to be exactly `planned` after the premium on
/// it and `network_fee`: `ceil((planned + network_fee) x 10^4 / (10^4 - premium_bps))`.
pub fn expected_received(
    settings: &PoolSettings,
    planned: TokenAmount,
    network_fee: TokenAmount,
) -> Option<TokenAmount> {
    let kept = BPS.checked_sub(u32::from(settings.premium_bps))?;
    planned
        .checked_add(network_fee)?
        .checked_mul(BPS)?
        .checked_div_ceil(kept)
}

/// The least ckUSDC the sale may make: the expected sale less half the slippage, rounded up.
pub fn sale_floor(expected: TokenAmount, slippage_bps: u16) -> Option<TokenAmount> {
    let kept = (2 * BPS).checked_sub(u32::from(slippage_bps))?;
    expected.checked_mul(kept)?.checked_div_ceil(2 * BPS)
}

/// The payout of a sale that made `received`: less the premium on it (rounded up) and the
/// network fee. None when they take all of it.
pub fn payout_of(
    settings: &PoolSettings,
    received: TokenAmount,
    network_fee: TokenAmount,
) -> Option<TokenAmount> {
    received
        .checked_sub(premium_share(settings, received)?)?
        .checked_sub(network_fee)
        .filter(|left| *left > TokenAmount::ZERO)
}

/// Whether `payout` is within the window of `planned`: `|payout - planned| x 10^4 <=
/// planned x 500`.
pub fn fits(planned: TokenAmount, payout: TokenAmount) -> bool {
    let gap = planned
        .checked_sub(payout)
        .or_else(|| payout.checked_sub(planned));
    let (Some(gap), Some(room)) = (gap, planned.checked_mul(WINDOW_BPS)) else {
        return false;
    };
    gap.checked_mul(BPS).is_some_and(|gap| gap <= room)
}

/// What the Base pocket must hold free behind an empty queue for a planned swap to open:
/// the plan's amount and 5%, rounded up.
pub fn open_cover(planned: TokenAmount) -> Option<TokenAmount> {
    planned.checked_mul(BPS + WINDOW_BPS)?.checked_div_ceil(BPS)
}

/// The least the plan's first spending asks of the Runner: the `min` of its first step that
/// spends the order's token in (the Runner gives that step all of it), or, with no step, the
/// payout's own `minOut` when it pays that token. A payout below it can only revert.
pub fn first_minimum(order: &crate::swapic::Order) -> TokenAmount {
    let token_in = order.tokenIn;
    let least = match order.steps.iter().find(|step| step.token == token_in) {
        Some(step) => step.min,
        None if order.steps.is_empty() && order.payout.token == token_in => order.payout.minOut,
        None => alloy_primitives::U256::ZERO,
    };
    TokenAmount::from_u256(least)
}
