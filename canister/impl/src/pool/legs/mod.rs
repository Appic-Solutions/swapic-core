//! One ICPSwap leg of any path, whatever it sells and buys (interface section 3c): the
//! second leg of a payout through ICP and its way back, and the legs that bring an ICP
//! user's token to ckUSDC (`swap_from_icp`). Each leg is recorded before its call, under the
//! payout it belongs to and its own leg number:
//!
//! - in one round, or beside the payout's pull when no leg has been at the pool since
//!   ([`crate::pool::payout::ReadAhead`]): its pool's quote, what the pool holds of the
//!   canister's before the swap (read while this leg alone is at the pool), and for a token
//!   whose approval may stand what the pool may still pull. A quote below the least it must
//!   give answers [`LegEnd::Short`] and moves nothing (what to do is the caller's: decline
//!   once, then fall back);
//! - the approval (`SwapApproved`), unless a standing one covers this swap: exactly the swap for
//!   ckUSDC and ckETH, [`STANDING_SWAPS`] of them for a token that may stand
//!   ([`crate::pool::payout::may_stand`]);
//! - `SwapBegun` with that reading and the amount in, and `depositFromAndSwap`;
//! - its output (`Swapped`), or, for a swap with no answer or the pool's own refusal, what the
//!   pool holds of the canister's above that reading: its output there is withdrawn and the
//!   leg paid; its deposit there is withdrawn back, never more than it put in (`SwapUndone`);
//!   nothing there after an answer that never came stops for a human.
//!
//! The caller holds the pool for this leg alone ([`crate::pool::payout`]'s leg lock).

#[cfg(test)]
mod tests;

use crate::ic::CallError;
use crate::pool::payout::{call_stop, open, record, Ledgers, Stop, STANDING_SWAPS};
use candid::Principal;
use futures::future::join3;
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

/// What a leg deposits once `approvals` approvals were given before its swap began:
/// the path budgets one, and each one more (a run that stopped between its approval and its
/// swap, then ran again) took one more fee of the canister's account, which the user pays out
/// of what the leg deposits. None when they take all of it.
pub fn after_approvals(
    amount_in: TokenAmount,
    fee_in: TokenAmount,
    approvals: Option<u32>,
) -> Option<TokenAmount> {
    fee_in
        .checked_mul(approvals.unwrap_or(0))
        .and_then(|extra| amount_in.checked_sub(extra))
        .filter(|left| *left > TokenAmount::ZERO)
}

/// A leg's reads, in one round. What the pool holds of the canister's before its deposit: only
/// what is there above it is ever the leg's. An allowance that cannot be read is an approval
/// asked again, never a swap that leans on it.
pub(crate) async fn read_round(
    ledgers: &impl Ledgers,
    pool: Principal,
    token_in: Principal,
    amount_in: TokenAmount,
    zero_for_one: bool,
    stands: bool,
) -> (
    Result<TokenAmount, CallError>,
    Result<PoolHeld, Stop>,
    Option<TokenAmount>,
) {
    join3(
        ledgers.quote(pool, amount_in, zero_for_one),
        held_at(ledgers, pool, zero_for_one),
        async {
            if stands {
                ledgers.allowance(token_in, pool).await.ok()
            } else {
                None
            }
        },
    )
    .await
}

/// Runs the leg from where `state` stands, as far as it goes now.
pub async fn run(
    ledgers: &impl Ledgers,
    spec: LegSpec,
    state: SwapLeg,
    clock: impl Fn() -> Timestamp + Copy,
    guard: &impl Fn() -> Result<(), String>,
) -> Result<LegEnd, Stop> {
    run_beside(ledgers, spec, state, clock, guard, |_| {}).await
}

/// [`run`], and `beside` told the leg's quote just as its swap goes out. Only a swap that goes
/// out now tells it: one that was begun before this run, or stops short, does not.
pub async fn run_beside(
    ledgers: &impl Ledgers,
    spec: LegSpec,
    state: SwapLeg,
    clock: impl Fn() -> Timestamp + Copy,
    guard: &impl Fn() -> Result<(), String>,
    beside: impl FnOnce(TokenAmount),
) -> Result<LegEnd, Stop> {
    if let Some(out) = state.out {
        return Ok(LegEnd::Out(out));
    }
    if state.begun {
        if state.undone.is_some() {
            return Ok(LegEnd::NotDone("the swap did not happen".to_string()));
        }
        // what it deposited is what its begin recorded
        let spec = LegSpec {
            amount_in: state.amount_in.unwrap_or(spec.amount_in),
            ..spec
        };
        return settle(ledgers, spec, state.before, None, clock).await;
    }
    // an approval given before this run is one fee more: the user's
    let spec = LegSpec {
        amount_in: after_approvals(spec.amount_in, spec.fee_in, state.approvals).ok_or_else(
            || Stop::Failed("the approvals given take all the leg deposits".to_string()),
        )?,
        ..spec
    };
    let Some(zero_for_one) = ledgers
        .direction(spec.pool, spec.token_in, spec.token_out)
        .await
        .map_err(|error| call_stop("the pool's metadata", error))?
    else {
        return Ok(LegEnd::NoPair);
    };
    let allowance = spec
        .amount_in
        .checked_add(spec.fee_in)
        .ok_or_else(|| Stop::Failed("the allowance overflows".to_string()))?;
    let stands = ledgers.may_stand(spec.token_in);
    // a reading beside the pull can start a sale, never end one: a refund cannot be taken back, so
    // a price below the least is asked again now, before it decides anything
    let read_ahead = ledgers
        .round_read_ahead(&spec)
        .filter(|read| read.quoted >= spec.least);
    let (quoted, before, allowed) = match read_ahead {
        Some(read) => (Ok(read.quoted), Ok(read.before), read.allowed),
        None => {
            read_round(
                ledgers,
                spec.pool,
                spec.token_in,
                spec.amount_in,
                zero_for_one,
                stands,
            )
            .await
        }
    };
    let quoted = quoted.map_err(|error| call_stop("the pool's quote", error))?;
    if quoted < spec.least {
        return Ok(LegEnd::Short(quoted));
    }
    let before = before?;
    open(guard)?;
    if allowed.is_none_or(|allowed| allowed < allowance) {
        let approved = if stands {
            let standing = allowance.checked_mul(STANDING_SWAPS).unwrap_or(allowance);
            match ledgers
                .approve_standing(spec.token_in, spec.pool, standing)
                .await
            {
                // a ledger that will not take a standing approval still sells: this swap alone
                Err(CallError::Refused(_)) => {
                    ledgers.approve(spec.token_in, spec.pool, allowance).await
                }
                answer => answer,
            }
        } else {
            ledgers.approve(spec.token_in, spec.pool, allowance).await
        };
        approved.map_err(|error| call_stop("the approval", error))?;
        record(
            PoolStep::SwapApproved {
                r#ref: spec.id,
                leg: spec.leg,
            },
            clock(),
        )?;
        open(guard)?;
    }
    record(
        PoolStep::SwapBegun {
            r#ref: spec.id,
            before: Some(before),
            leg: spec.leg,
            amount_in: Some(spec.amount_in),
        },
        clock(),
    )?;
    beside(quoted);
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
/// deposit and the pull's fee, or what the deposit cost in the end once undone. A begun leg with
/// no approval counted gave one before approvals were counted, or leaned on a standing one: that
/// one is budgeted too, so the sum is at worst a fee above what was spent, and a refund never
/// pays out more than the swap brought in.
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

/// How a chained quote's refusals name its two pools and what the swap needs: each lands in
/// a `SwapDeclined` line or an undo's why as it reads.
pub struct ChainNames {
    pub first: &'static str,
    pub second: &'static str,
    pub need: &'static str,
}

/// The least the first of two legs must give, from both pools' quotes: the second leg's quote
/// for what the first buys (less `fees`) must reach `least`; below it, why.
pub async fn chained_least(
    ledgers: &impl Ledgers,
    (first, token_in, amount_in): (Principal, Principal, TokenAmount),
    (second, middle, token_out): (Principal, Principal, Principal),
    fees: TokenAmount,
    least: TokenAmount,
    names: ChainNames,
) -> Result<Result<TokenAmount, String>, Stop> {
    let Some(zero_for_one) = ledgers
        .direction(first, token_in, middle)
        .await
        .map_err(|error| call_stop("the pool's metadata", error))?
    else {
        return Ok(Err(format!("{} does not trade the pair", names.first)));
    };
    let bought = ledgers
        .quote(first, amount_in, zero_for_one)
        .await
        .map_err(|error| call_stop("the pool's quote", error))?;
    let sold = bought.checked_sub(fees).unwrap_or(TokenAmount::ZERO);
    let Some(zero_for_one) = ledgers
        .direction(second, middle, token_out)
        .await
        .map_err(|error| call_stop("the pool's metadata", error))?
    else {
        return Ok(Err(format!("{} does not trade the pair", names.second)));
    };
    let quoted = ledgers
        .quote(second, sold, zero_for_one)
        .await
        .map_err(|error| call_stop("the pool's quote", error))?;
    if sold == TokenAmount::ZERO || quoted < least {
        return Ok(Err(format!(
            "the pools quote {quoted} through ICP, below the {least} {}",
            names.need
        )));
    }
    first_leg_least(sold, quoted, least, fees)
        .map(Ok)
        .ok_or_else(|| Stop::Failed("the first leg's minimum overflows".to_string()))
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
