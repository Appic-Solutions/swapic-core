//! One ICPSwap leg of any path, whatever it sells and buys (interface section 3c): the
//! second leg of a payout through ICP and its way back, and the legs that bring an ICP
//! user's token to ckUSDC (`swap_from_icp`). Each leg is recorded before its call, under the
//! payout it belongs to and its own leg number:
//!
//! - its pool's quote first, below the least it must give answers [`LegEnd::Short`] and
//!   calls nothing (what to do is the caller's: decline once, then fall back);
//! - what the pool holds of the canister's before the swap, read while this leg alone is at
//!   the pool, then the approval (`SwapApproved`), `SwapBegun` with that reading and the
//!   amount in, and `depositFromAndSwap`;
//! - its output (`Swapped`), or, for a swap with no answer or the pool's own refusal, what the
//!   pool holds of the canister's above that reading: its output there is withdrawn and the
//!   leg paid; its deposit there is withdrawn back, never more than it put in (`SwapUndone`);
//!   nothing there after an answer that never came stops for a human.
//!
//! The caller holds the pool for this leg alone ([`crate::pool::payout`]'s leg lock).

#[cfg(test)]
mod tests;

use crate::ic::CallError;
use crate::pool::payout::{call_stop, open, Ledgers, Stop};
use crate::storage::pool as pool_store;
use candid::Principal;
use types::hash::PoolRef;
use types::pool::{PoolHeld, PoolStep, SwapLeg};
use types::{Timestamp, TokenAmount};

/// One leg as its caller fixes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LegSpec {
    /// The payout its steps name.
    pub id: PoolRef,
    /// Which leg of the payout: none for the first.
    pub leg: Option<u8>,
    pub pool: Principal,
    pub token_in: Principal,
    pub token_out: Principal,
    /// What it deposits, its pull's fee aside.
    pub amount_in: TokenAmount,
    /// The least output the swap must give.
    pub least: TokenAmount,
    pub fee_in: TokenAmount,
    pub fee_out: TokenAmount,
    /// Whether its output goes on to a recipient: its transfer then takes one more fee.
    pub pays_out: bool,
}

impl LegSpec {
    /// What the output pays on: the pool's withdrawal to the canister takes one fee, and a
    /// transfer on to a recipient another.
    pub fn send_of(&self, out: TokenAmount) -> TokenAmount {
        let fees = if self.pays_out {
            self.fee_out.checked_mul(2_u8)
        } else {
            Some(self.fee_out)
        };
        fees.and_then(|fees| out.checked_sub(fees))
            .unwrap_or(TokenAmount::ZERO)
    }
}

/// How a leg ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LegEnd {
    /// It swapped `out` of its token out (the canister holds it, less the withdrawal's fee).
    Out(TokenAmount),
    /// The pool quotes `quoted`, below the least: nothing was called.
    Short(TokenAmount),
    /// The pool does not trade the pair: nothing was called.
    NoPair,
    /// The swap did not happen; its deposit, if any, came back (`SwapUndone`).
    NotDone(String),
}

fn record(step: PoolStep, now: Timestamp) -> Result<(), Stop> {
    pool_store::record_at(step, now)
        .map(drop)
        .map_err(|error| Stop::Append(error.to_string()))
}

/// What the pool holds of the canister's, unused, as the leg's token in and token out.
async fn held_at(
    ledgers: &impl Ledgers,
    pool: Principal,
    zero_for_one: bool,
) -> Result<PoolHeld, Stop> {
    let (unused0, unused1) = ledgers
        .unused(pool)
        .await
        .map_err(|error| call_stop("the pool's unused balances", error))?;
    Ok(if zero_for_one {
        PoolHeld {
            ckusdc: unused0,
            token: unused1,
        }
    } else {
        PoolHeld {
            ckusdc: unused1,
            token: unused0,
        }
    })
}

/// Runs the leg from where `state` stands, as far as it goes now.
pub async fn run(
    ledgers: &impl Ledgers,
    spec: LegSpec,
    state: SwapLeg,
    clock: impl Fn() -> Timestamp + Copy,
    guard: &impl Fn() -> Result<(), String>,
) -> Result<LegEnd, Stop> {
    if let Some(out) = state.out {
        return Ok(LegEnd::Out(out));
    }
    if state.begun {
        if state.undone.is_some() {
            return Ok(LegEnd::NotDone("the swap did not happen".to_string()));
        }
        return settle(ledgers, spec, state.before, None, clock).await;
    }
    let Some(zero_for_one) = ledgers
        .direction(spec.pool, spec.token_in, spec.token_out)
        .await
        .map_err(|error| call_stop("the pool's metadata", error))?
    else {
        return Ok(LegEnd::NoPair);
    };
    let quoted = ledgers
        .quote(spec.pool, spec.amount_in, zero_for_one)
        .await
        .map_err(|error| call_stop("the pool's quote", error))?;
    if quoted < spec.least {
        return Ok(LegEnd::Short(quoted));
    }
    let allowance = spec
        .amount_in
        .checked_add(spec.fee_in)
        .ok_or_else(|| Stop::Failed("the allowance overflows".to_string()))?;
    // what the pool holds of the canister's before this leg's deposit, read while this leg
    // alone is at the pool: only what is there above it is ever this leg's
    let before = held_at(ledgers, spec.pool, zero_for_one).await?;
    open(guard)?;
    ledgers
        .approve(spec.token_in, spec.pool, allowance)
        .await
        .map_err(|error| call_stop("the approval", error))?;
    record(
        PoolStep::SwapApproved {
            r#ref: spec.id,
            leg: spec.leg,
        },
        clock(),
    )?;
    open(guard)?;
    record(
        PoolStep::SwapBegun {
            r#ref: spec.id,
            before: Some(before),
            leg: spec.leg,
            amount_in: Some(spec.amount_in),
        },
        clock(),
    )?;
    // begun: whatever this answers, the swap is never called again
    match ledgers
        .swap(
            spec.pool,
            spec.amount_in,
            zero_for_one,
            spec.least,
            spec.fee_in,
            spec.fee_out,
        )
        .await
    {
        Ok(out) => {
            record(
                PoolStep::Swapped {
                    r#ref: spec.id,
                    out,
                    send: Some(spec.send_of(out)),
                    leg: spec.leg,
                },
                clock(),
            )?;
            Ok(LegEnd::Out(out))
        }
        Err(CallError::Refused(why)) => settle(ledgers, spec, Some(before), Some(why), clock).await,
        Err(_) => settle(ledgers, spec, Some(before), None, clock).await,
    }
}

/// A begun swap that did not answer with its output: what the pool holds of the canister's
/// above what it held just before tells what happened, and only that is this leg's.
async fn settle(
    ledgers: &impl Ledgers,
    spec: LegSpec,
    before: Option<PoolHeld>,
    refused: Option<String>,
    clock: impl Fn() -> Timestamp + Copy,
) -> Result<LegEnd, Stop> {
    let unknown = || Stop::Failed("the ICPSwap swap's outcome is unknown".to_string());
    let before = before.ok_or_else(unknown)?;
    let zero_for_one = ledgers
        .direction(spec.pool, spec.token_in, spec.token_out)
        .await
        .map_err(|error| call_stop("the pool's metadata", error))?
        .ok_or_else(unknown)?;
    let now = held_at(ledgers, spec.pool, zero_for_one).await?;
    let above = |now: TokenAmount, before: TokenAmount| {
        now.checked_sub(before).unwrap_or(TokenAmount::ZERO)
    };
    let (in_left, out_left) = (
        above(now.ckusdc, before.ckusdc),
        above(now.token, before.token),
    );
    // withdrawing from the pool brings money in: no guard
    if out_left > TokenAmount::ZERO {
        ledgers
            .withdraw(spec.pool, spec.token_out, out_left, spec.fee_out)
            .await
            .map_err(|error| call_stop("the output's withdrawal from the pool", error))?;
        record(
            PoolStep::Swapped {
                r#ref: spec.id,
                out: out_left,
                send: Some(spec.send_of(out_left)),
                leg: spec.leg,
            },
            clock(),
        )?;
        return Ok(LegEnd::Out(out_left));
    }
    let deposit = spec
        .amount_in
        .checked_add(spec.fee_in)
        .ok_or_else(|| Stop::Failed("the deposit overflows".to_string()))?;
    if in_left >= spec.amount_in || (refused.is_some() && in_left > TokenAmount::ZERO) {
        // this leg's own deposit, never more
        let withdrawn = in_left.min(spec.amount_in);
        ledgers
            .withdraw(spec.pool, spec.token_in, withdrawn, spec.fee_in)
            .await
            .map_err(|error| call_stop("the deposit's withdrawal from the pool", error))?;
        let returned = withdrawn
            .checked_sub(spec.fee_in)
            .unwrap_or(TokenAmount::ZERO);
        let cost = deposit.checked_sub(returned).unwrap_or(TokenAmount::ZERO);
        record(
            PoolStep::SwapUndone {
                r#ref: spec.id,
                cost,
                withdrawn,
                leg: spec.leg,
            },
            clock(),
        )?;
        return Ok(LegEnd::NotDone(format!(
            "the swap did not happen; {returned} came back from the pool"
        )));
    }
    match refused {
        Some(why) => {
            record(
                PoolStep::SwapUndone {
                    r#ref: spec.id,
                    cost: TokenAmount::ZERO,
                    withdrawn: TokenAmount::ZERO,
                    leg: spec.leg,
                },
                clock(),
            )?;
            Ok(LegEnd::NotDone(format!(
                "the pool refused the swap before its deposit: {why}"
            )))
        }
        None => Err(unknown()),
    }
}

/// What a leg cost the account it sold from: every approval's fee, and once begun its
/// deposit and the pull's fee, or what the deposit cost in the end once undone.
pub fn cost_in(state: &SwapLeg, fee_in: TokenAmount) -> Option<TokenAmount> {
    let approved = fee_in.checked_mul(state.approvals.unwrap_or(u32::from(state.begun)))?;
    if !state.begun {
        return Some(approved);
    }
    let deposit = match state.undone {
        Some(cost) => cost,
        None => state.amount_in?.checked_add(fee_in)?,
    };
    approved.checked_add(deposit)
}

/// The least a first leg must give so its output, less the fees the second leg's pull takes
/// (`fees`), buys at least `least` on a second leg quoted `quoted` for `quoted_in`: the
/// second leg's input scaled to `least` (a pool's output per unit falls as its input grows,
/// so the scaled input buys at least `least`), rounded up, plus the fees.
pub fn first_leg_least(
    quoted_in: TokenAmount,
    quoted: TokenAmount,
    least: TokenAmount,
    fees: TokenAmount,
) -> Option<TokenAmount> {
    if quoted == TokenAmount::ZERO {
        return None;
    }
    quoted_in
        .checked_mul(least.into_inner())?
        .checked_div_ceil(quoted.into_inner())?
        .checked_add(fees)
}
