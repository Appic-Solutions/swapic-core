//! `resolve_deposit` (Jeff, 2026-10-24): an EVM to ICP deposit whose payout is held for a
//! human (`DepositStatus::PayoutFailed`): a started payout after its tenth failure, a queued
//! one that never started (CR-08), a stake whose mint kept failing. Its lines stayed held,
//! since nothing led out of it. A controller, having looked at the ledgers, resolves it once:
//!
//! - `Retry`: back to its payout with fresh attempts. A started one pays again with its own
//!   dedup instant while the ledger still deduplicates it (a transfer whose answer was lost,
//!   and that went out, is then the same transfer), else with the resolution's; one that never
//!   started is queued again, at the back; a stake mints again.
//! - `Refund`: what arrived goes back to its payer on Base, less the least network fee,
//!   through the orphan refund path, behind the pool's guard: its arrival is an orphan held
//!   under its transfer's key, and the canister signs `USDC.transfer(payer, amount - fee)`. A
//!   signature that fails leaves the orphan held, for `resolve_orphan`. A payout whose ICPSwap
//!   leg moved money is never refunded whole, and a deposit with no payer is not refunded.
//!
//! The canister cannot tell a ckUSDC transfer whose answer was lost: before a `Refund`, or a
//! `Retry` past the window, the controller checks no transfer of the payout went out.

#[cfg(test)]
mod tests;

use crate::pool::orphans::{self, Refund};
use crate::pool::payout::{Busy, DEDUP_WINDOW};
use crate::storage::pool as pool_store;
use settlement_api::types::pool::{DepositResolution as WireResolution, PoolError};
use types::hash::PoolRef;
use types::pool::{DepositResolution, DepositStatus, PoolStep};
use types::{Address, EvmAddress, Timestamp};

/// The dedup instant a started payout is retried with: its attempt's own while a ledger
/// still deduplicates it, else `now`.
pub fn retry_at(attempt_at: Timestamp, now: Timestamp) -> Timestamp {
    if now.saturating_duration_since(attempt_at) < DEDUP_WINDOW {
        attempt_at
    } else {
        now
    }
}

fn record(step: PoolStep, now: Timestamp) -> Result<(), PoolError> {
    pool_store::record_at(step, now)
        .map(drop)
        .map_err(|error| PoolError::NotAllowed(error.to_string()))
}

/// `resolve_deposit`'s body, the guards given (`open`: the pool's guard): for a refund, the
/// transfer to sign, every check of it made before its line.
pub fn resolve(
    halted: bool,
    is_controller: bool,
    r#ref: PoolRef,
    resolution: WireResolution,
    now: Timestamp,
    sanctioned: impl Fn(&Address) -> bool,
    open: impl Fn() -> Result<(), PoolError>,
) -> Result<Option<Refund>, PoolError> {
    if halted {
        return Err(PoolError::Halted);
    }
    if !is_controller {
        return Err(PoolError::NotAllowed(
            "only a controller resolves a deposit".to_string(),
        ));
    }
    let deposit = pool_store::deposit(&r#ref).ok_or(PoolError::Unknown)?;
    if !matches!(deposit.status, DepositStatus::PayoutFailed(_)) {
        return Err(PoolError::NotAllowed(format!(
            "the deposit is {:?}, not held for a human",
            deposit.status
        )));
    }
    let _held = Busy::take(r#ref.into_bytes())
        .ok_or_else(|| PoolError::Unavailable("the deposit is being driven now".to_string()))?;
    match resolution {
        WireResolution::Retry => {
            let retry_at = deposit
                .payout
                .attempt
                .map(|attempt| retry_at(attempt.created_at, now));
            record(
                PoolStep::DepositResolved {
                    r#ref,
                    resolution: DepositResolution::Retry,
                    retry_at,
                },
                now,
            )?;
            Ok(None)
        }
        WireResolution::Refund => {
            let (key, orphan) = deposit
                .refund_orphan()
                .ok_or_else(|| PoolError::NotAllowed("nothing arrived".to_string()))?;
            let payer = deposit.terms.src_payer;
            if payer == EvmAddress::ZERO {
                return Err(PoolError::NotAllowed(
                    "the deposit has no payer to refund".to_string(),
                ));
            }
            if deposit.payout.moved_on_icp() {
                return Err(PoolError::NotAllowed(
                    "its payout moved money on ICP: it is not refunded whole".to_string(),
                ));
            }
            let refund = orphans::refund_call(key, &orphan, payer, sanctioned, open)?;
            record(
                PoolStep::DepositResolved {
                    r#ref,
                    resolution: DepositResolution::Refund,
                    retry_at: None,
                },
                now,
            )?;
            Ok(Some(refund))
        }
    }
}
