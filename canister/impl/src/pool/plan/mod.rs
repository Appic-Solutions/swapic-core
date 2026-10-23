//! The payout plan's fit (2026-10-23): the moment an ICP swap's sale made its exact ckUSDC and
//! its payout waits with its USDC reserved, the canister fills the real amount into the plan
//! the user handed in and signs it itself, in the same message chain: no watcher, no quoter,
//! no `pay_payout`. The plan fits when the payout is within 5% of the plan's amount, the
//! plan's order deadline is at least [`ORDER_MARGIN_S`] ahead, and its first step asks no more
//! than the payout.
//!
//! A KyberSwap step on Base is rewritten for the real amount by KyberSwap's
//! InputScalingHelper, read with one `eth_call` once the amount is final (after the pull for
//! ckUSDC, after the last leg's answer for a token: never beside the pull, which anyone could
//! make the canister pay for). The rewritten step must still be the same swap: the same
//! tokens, executor and receivers, no fee and no permit, selling all of the amount, for at
//! least its old minimum scaled to it. The whole order then meets every check a payout order
//! meets ([`crate::pool::pay::check_order`], the amount exact).
//!
//! A plan that does not fit leaves the payout waiting for the watcher's order, the way every
//! payout waited before the plan flow; one whose first step asks more than the payout could
//! only revert, so the payout is undone at once.

#[cfg(test)]
mod tests;

use crate::evm_reads::{self, Simulated};
use crate::pool::pay::{self, ORDER_MARGIN_S};
use alloy_primitives::U256;
use settlement_api::types::pool::PoolError;
use types::pool::payout::{EvmPayout, PayoutKind, PinnedOrder};
use types::pool::plan;
use types::swapic::{self, KyberSwap, Order, Step, KYBER_SCALER, KYBER_SWAP};
use types::{Timestamp, TokenAmount};

/// The most a scaler's answer may be: a route of 30 KiB, hex.
const MAX_SCALED_BYTES: u64 = 64 * 1_024;

/// What fitting a plan reads beyond the fold, so the drive runs the same against the chain
/// and a fake.
#[allow(async_fn_in_trait)]
pub trait PlanReads {
    /// A KyberSwap route rewritten for `amount` by KyberSwap's InputScalingHelper on the pool
    /// chain; Err with why when it was not.
    async fn scaled(&self, route: &[u8], amount: TokenAmount) -> Result<Vec<u8>, String>;

    /// Every check of a payout order, against the config, the canister's address and the
    /// sanctions as they are now.
    fn checked(
        &self,
        payout: &EvmPayout,
        order: &Order,
        order_bytes: &[u8],
        gas_limit: u64,
        now: Timestamp,
    ) -> Result<PinnedOrder, PoolError>;
}

impl PlanReads for crate::pool::payout::Live {
    async fn scaled(&self, route: &[u8], amount: TokenAmount) -> Result<Vec<u8>, String> {
        let chain = crate::storage::config::get()
            .pool_chain
            .ok_or("no pool chain is configured")?;
        let canister =
            crate::storage::ecdsa_address::get().ok_or("the canister's address is not derived")?;
        let call = swapic::scale_calldata(route, U256::from_be_bytes(amount.to_be_bytes()));
        match evm_reads::eth_call_capped(chain, canister, KYBER_SCALER, &call, scaled_cap(route))
            .await
        {
            Ok(Simulated::Returned(answer)) => scaled_of(&answer),
            Ok(Simulated::Reverted(_)) => Err("the scaler's read reverted".to_string()),
            // the kind only: an outcall's text can carry the provider's url and key
            Err(error) => Err(format!("the scaler's read: {:?}", error.kind())),
        }
    }

    fn checked(
        &self,
        payout: &EvmPayout,
        order: &Order,
        order_bytes: &[u8],
        gas_limit: u64,
        now: Timestamp,
    ) -> Result<PinnedOrder, PoolError> {
        let canister = crate::storage::ecdsa_address::get().ok_or_else(|| {
            PoolError::Unavailable("the canister's address is not derived yet".to_string())
        })?;
        pay::checked_payout(payout, order, order_bytes, gas_limit, canister, now)
    }
}

/// How much of a scaler's answer to `route` is read: the rewritten route as hex, its words
/// and the JSON around it.
pub fn scaled_cap(route: &[u8]) -> u64 {
    u64::try_from(route.len())
        .unwrap_or(u64::MAX)
        .saturating_add(160)
        .saturating_mul(2)
        .saturating_add(1_024)
        .min(MAX_SCALED_BYTES)
}

/// The rewritten route a scaler's answer carries; Err with why for its refusal or an answer
/// that does not decode.
pub fn scaled_of(answer: &[u8]) -> Result<Vec<u8>, String> {
    match swapic::scaled_route(answer) {
        Ok(Some(route)) => Ok(route),
        Ok(None) => Err("the scaler refused the amount".to_string()),
        Err(error) => Err(error.to_string()),
    }
}

/// What a plan does with a payout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fit {
    /// Signed next, with this order and its pin.
    Sign(Box<Fitted>),
    /// Waits for the watcher's order, as before the plan flow: why.
    Wait(String),
    /// Undone at once: why.
    Undo(String),
}

/// A plan filled with the payout's real amount, checked: what is signed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fitted {
    pub order: Order,
    pub pinned: PinnedOrder,
}

/// What a plan says of a payout of `amount` at `now_s`, before any read, pure: the order with
/// that amount in and the place of its KyberSwap step, if it has one; or why it waits, or is
/// undone.
pub fn decide(
    plan: &PinnedOrder,
    planned: TokenAmount,
    amount: TokenAmount,
    now_s: u64,
) -> Result<(Order, Option<usize>), Fit> {
    let mut order = swapic::decode_order(&plan.order)
        .ok_or_else(|| Fit::Wait("the plan is not abi.encode of an Order".to_string()))?;
    // a plan that could only revert undoes the payout, whatever else it says
    let first = plan::first_minimum(&order);
    if first > amount {
        return Err(Fit::Undo(format!(
            "the plan's first step asks {first}, above the payout's {amount}"
        )));
    }
    if !plan::fits(planned, amount) {
        return Err(Fit::Wait(format!(
            "the payout {amount} is more than 5% from the plan's {planned}"
        )));
    }
    if plan.deadline_s < now_s.saturating_add(ORDER_MARGIN_S) {
        return Err(Fit::Wait(
            "the plan's order deadline is too close".to_string(),
        ));
    }
    let swaps: Vec<usize> = (0..order.steps.len())
        .filter(|at| swapic::selector_of(&order.steps[*at]) == Some(KYBER_SWAP))
        .collect();
    if swaps.len() > 1 {
        return Err(Fit::Wait("the plan swaps more than once".to_string()));
    }
    order.amountIn = U256::from_be_bytes(amount.to_be_bytes());
    Ok((order, swaps.first().copied()))
}

/// The plan's KyberSwap `step` rewritten to `route` for `amount`, pure: the same swap but for
/// its amounts (the same tokens, executor and receivers, no fee, no permit), selling all of
/// `amount`, for at least its old minimum scaled to it (rounded down, as the scaler rounds).
pub fn rescaled(step: &Step, route: Vec<u8>, amount: TokenAmount) -> Result<Step, String> {
    let swap_of = |step: &Step| {
        KyberSwap::of_step(step)
            .and_then(Result::ok)
            .ok_or_else(|| "the step is no KyberSwap swap the canister reads".to_string())
    };
    let before = swap_of(step)?;
    let rewritten = Step {
        data: route.into(),
        ..step.clone()
    };
    let after = swap_of(&rewritten)?;
    let amounts_aside = KyberSwap {
        amount: before.amount,
        min_return: before.min_return,
        ..after.clone()
    };
    if amounts_aside != before {
        return Err("the rewritten route is another swap".to_string());
    }
    let amount = U256::from_be_bytes(amount.to_be_bytes());
    if after.amount != amount {
        return Err(format!(
            "the rewritten route sells {}, not {amount}",
            after.amount
        ));
    }
    let least = before
        .min_return
        .checked_mul(amount)
        .and_then(|scaled| scaled.checked_div(before.amount))
        .ok_or("the old minimum does not scale")?;
    if after.min_return < least {
        return Err(format!(
            "the rewritten route guarantees {}, below the {least} its old minimum scales to",
            after.min_return
        ));
    }
    Ok(rewritten)
}

/// Fits `payout`'s plan to its exact amount at `now`: the scaler read (once, for a KyberSwap
/// step), the rewritten step's checks, then every check of a payout order.
pub async fn fit(reads: &impl PlanReads, payout: &EvmPayout, now: Timestamp) -> Fit {
    let (Some(plan), PayoutKind::IcpSwap(terms), Some(amount)) =
        (payout.plan.as_ref(), payout.kind, payout.amount)
    else {
        return Fit::Wait("the payout has no plan".to_string());
    };
    let Some(planned) = terms.planned else {
        return Fit::Wait("the payout has no plan".to_string());
    };
    let (mut order, swap) = match decide(plan, planned, amount, now.as_secs().get()) {
        Ok(decided) => decided,
        Err(fit) => return fit,
    };
    if let Some(at) = swap {
        let route = match reads.scaled(&order.steps[at].data, amount).await {
            Ok(route) => route,
            Err(why) => return Fit::Wait(format!("the plan's swap was not rewritten: {why}")),
        };
        order.steps[at] = match rescaled(&order.steps[at], route, amount) {
            Ok(step) => step,
            Err(why) => return Fit::Wait(why),
        };
    }
    // the payout as it stands after the read
    let Some(payout) = crate::storage::pool::payout(payout.id) else {
        return Fit::Wait("the payout is gone".to_string());
    };
    let bytes = swapic::encode_order(&order);
    match reads.checked(&payout, &order, &bytes, plan.gas_limit, now) {
        Ok(pinned) => Fit::Sign(Box::new(Fitted { order, pinned })),
        Err(error) => Fit::Wait(format!("the fitted plan is refused: {error:?}")),
    }
}
