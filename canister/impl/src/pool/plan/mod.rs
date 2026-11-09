//! The payout plan's fit: the moment an ICP swap's sale made its exact ckUSDC and
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
//! The reads a fit needs (that rewrite, and the gas price its signature needs) cost the swap a
//! round each after its sale, so a token's sale starts them beside itself, in a task of their
//! own, for the amount its quote says ([`PlanReads::read_fit_ahead`]). A rewrite read ahead is
//! taken only for exactly the amount the sale made and the very route the fit holds, and goes
//! through every check a fresh one does: the scaler is a pure function of the two. Never beside
//! the pull: anyone can make a door pull, and an outcall is the canister's to pay for.
//!
//! A plan that does not fit leaves the payout waiting for the watcher's order, the way every
//! payout waited before the plan flow; one whose first step asks more than the payout could
//! only revert, so the payout is undone at once.

#[cfg(test)]
mod tests;

use crate::evm_reads::{self, Simulated};
use crate::pool::pay::{self, ORDER_MARGIN_S};
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

    /// Makes sure the pool chain has a gas reading young enough to sign with. It answers
    /// nothing: a signature with no such reading reads for itself, and says why it could not.
    async fn gas_ready(&self) {}

    /// Starts the reads a fit for `amount` would make, for payout `id`, in a task of their own.
    /// `route` is the plan's swap to rewrite, if it has one. Nothing waits on them.
    fn read_fit_ahead(&self, _id: u64, _route: Option<Vec<u8>>, _amount: TokenAmount) {}

    /// `route` rewritten for exactly `amount`, if [`Self::read_fit_ahead`] read just that for
    /// payout `id`.
    fn scaled_ahead(&self, _id: u64, _route: &[u8], _amount: TokenAmount) -> Option<Vec<u8>> {
        None
    }

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

/// A rewrite read ahead: for which payout, of which route, for which amount, and what it gave.
struct ScaledAhead {
    id: u64,
    route: Vec<u8>,
    amount: TokenAmount,
    scaled: Vec<u8>,
}

thread_local! {
    // Heap by necessity, and nothing depends on it: the last rewrite read ahead, one at a time so
    // it costs a route's bytes at most. Gone or overwritten, a fit reads for itself.
    static SCALED_AHEAD: std::cell::RefCell<Option<ScaledAhead>> =
        const { std::cell::RefCell::new(None) };
}

impl PlanReads for crate::pool::payout::Live {
    async fn gas_ready(&self) {
        if let Some(chain) = crate::storage::config::get().pool_chain {
            if let Err(failure) = crate::gas::ensure_fresh(chain).await {
                crate::pool::log(&format!("the gas price read ahead: {failure:?}"));
            }
        }
    }

    fn read_fit_ahead(&self, id: u64, route: Option<Vec<u8>>, amount: TokenAmount) {
        // in a message of its own: this is called as the sale goes out, and a trap in a read that
        // is only ever a saving must not take the sale's own lines back with it
        #[cfg(target_arch = "wasm32")]
        ic_cdk_timers::set_timer(std::time::Duration::ZERO, move || {
            ic_cdk::spawn(async move {
                let live = crate::pool::payout::Live;
                let scale = async {
                    match &route {
                        Some(route) => live.scaled(route, amount).await.ok(),
                        None => None,
                    }
                };
                let ((), scaled) = futures::future::join(live.gas_ready(), scale).await;
                if let (Some(route), Some(scaled)) = (route, scaled) {
                    SCALED_AHEAD.with(|ahead| {
                        *ahead.borrow_mut() = Some(ScaledAhead {
                            id,
                            route,
                            amount,
                            scaled,
                        });
                    });
                }
            })
        });
        #[cfg(not(target_arch = "wasm32"))]
        let _ = (id, route, amount);
    }

    fn scaled_ahead(&self, id: u64, route: &[u8], amount: TokenAmount) -> Option<Vec<u8>> {
        SCALED_AHEAD.with(|ahead| {
            let mut ahead = ahead.borrow_mut();
            let hit = ahead
                .as_ref()
                .is_some_and(|read| read.id == id && read.amount == amount && read.route == route);
            hit.then(|| ahead.take().map(|read| read.scaled)).flatten()
        })
    }

    async fn scaled(&self, route: &[u8], amount: TokenAmount) -> Result<Vec<u8>, String> {
        let chain = crate::storage::config::get()
            .pool_chain
            .ok_or("no pool chain is configured")?;
        let canister =
            crate::storage::ecdsa_address::get().ok_or("the canister's address is not derived")?;
        let call = swapic::scale_calldata(route, amount.to_u256());
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

/// Why a plan does not go on to its fit as it stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fit {
    /// Waits for the watcher's order, as before the plan flow: why.
    Wait(String),
    /// Undone at once: why.
    Undo(String),
}

/// A plan decided for its payout before any read ([`decide_payout`]): the order with the
/// payout's exact amount in, the place of its KyberSwap step if it has one, that amount, and
/// the plan's gas limit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decided {
    pub order: Order,
    pub swap: Option<usize>,
    pub amount: TokenAmount,
    pub gas_limit: u64,
}

/// A plan filled with the payout's real amount, checked: what is signed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fitted {
    pub order: Order,
    pub pinned: PinnedOrder,
}

/// Counts a try of payout `id`'s plan that waits, with why: at most `MAX_FIT_TRIES` in all,
/// so a refused line (logged) only means none is left, or the payout moved on.
pub fn count_try(id: u64, why: String, now: Timestamp) {
    if let Err(error) =
        crate::storage::pool::record_at(types::pool::PoolStep::PayoutFitWaited { id, why }, now)
    {
        crate::pool::log(&format!(
            "the pool payout {id}'s try of its plan was not counted: {error}"
        ));
    }
}

/// Signs the plan fitted for payout `id`, its caller holding the payout (the door's drive,
/// the pass): a refusal (a halt, the gas rule, a stale reading) is a try counted (else every
/// pass refits it, a scaler read each time, and the watcher's relist never comes), and
/// leaves it waiting for an order, logged. Answers whether it was signed.
pub async fn sign_fitted(
    held: &crate::pool::payout::Busy,
    id: u64,
    fitted: Fitted,
    clock: impl Fn() -> Timestamp,
) -> bool {
    match pay::sign_held(held, id, &fitted.order, fitted.pinned).await {
        Ok(_) => true,
        Err(error) => {
            crate::pool::log(&format!(
                "the pool payout {id}'s plan was not signed ({error:?}): it waits for an order"
            ));
            count_try(id, format!("the plan was not signed: {error:?}"), clock());
            false
        }
    }
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
    order.amountIn = amount.to_u256();
    Ok((order, swaps.first().copied()))
}

/// [`decide`] for `payout`, its plan, planned amount and amount read off it: none for a
/// payout with no plan or no amount yet.
pub fn decide_payout(payout: &EvmPayout, now_s: u64) -> Option<Result<Decided, Fit>> {
    let (Some(plan), PayoutKind::IcpSwap(terms), Some(amount)) =
        (payout.plan.as_ref(), payout.kind, payout.amount)
    else {
        return None;
    };
    Some(
        decide(plan, terms.planned?, amount, now_s).map(|(order, swap)| Decided {
            order,
            swap,
            amount,
            gas_limit: plan.gas_limit,
        }),
    )
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
    let amount = amount.to_u256();
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

/// Fits payout `id`'s plan, as [`decide_payout`] decided it, to its exact amount at `now`:
/// the scaler read (once, for a KyberSwap step), the rewritten step's checks, then every check
/// of a payout order. The order to sign, or why the payout waits.
pub async fn fit(
    reads: &impl PlanReads,
    id: u64,
    decided: Decided,
    now: Timestamp,
) -> Result<Fitted, String> {
    let Decided {
        mut order,
        swap,
        amount,
        gas_limit,
    } = decided;
    if let Some(at) = swap {
        let route = match reads.scaled_ahead(id, &order.steps[at].data, amount) {
            Some(route) => route,
            None => reads
                .scaled(&order.steps[at].data, amount)
                .await
                .map_err(|why| format!("the plan's swap was not rewritten: {why}"))?,
        };
        order.steps[at] = rescaled(&order.steps[at], route, amount)?;
    }
    // the payout as it stands after the read
    let payout = crate::storage::pool::payout(id).ok_or("the payout is gone")?;
    let bytes = swapic::encode_order(&order);
    let pinned = reads
        .checked(&payout, &order, &bytes, gas_limit, now)
        .map_err(|error| format!("the fitted plan is refused: {error:?}"))?;
    Ok(Fitted { order, pinned })
}
