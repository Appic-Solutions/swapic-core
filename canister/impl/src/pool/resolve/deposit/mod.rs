//! `resolve_deposit` (Jeff, 2026-10-24): an EVM to ICP deposit whose payout is held for a
//! human (`DepositStatus::PayoutFailed`): a started payout after its tenth failure, a queued
//! one that never started (CR-08), a stake whose mint kept failing. Its lines stayed held,
//! since nothing led out of it. A controller resolves it once:
//!
//! - `Retry`: back to its payout with fresh attempts. A started one pays again with its own
//!   dedup pair, and only while the ledger still deduplicates it: a transfer that landed, its
//!   answer lost, is then answered `Duplicate` and recorded paid, once. Past that window the
//!   canister cannot tell, and the controller reads the ledger. One that never started is
//!   queued again, at the back; a stake mints again.
//! - `Refund`: only for a deposit whose payout never started (nothing went out on ICP; a
//!   started one's transfer may have landed, so it is never refunded on Base, the review,
//!   2026-10-24). What arrived goes back to its payer on Base, less the least network fee,
//!   through the orphan refund path, behind the pool's guard: its arrival is an orphan held
//!   under its transfer's key, and the canister signs `USDC.transfer(payer, amount - fee)`. A
//!   signature that fails leaves the orphan held, for `resolve_orphan`. A deposit with no
//!   payer is not refunded.

#[cfg(test)]
mod tests;

use crate::pool::orphans::{self, Refund};
use crate::pool::payout::{Busy, DEDUP_WINDOW};
use crate::storage::pool as pool_store;
use settlement_api::types::pool::{DepositResolution as WireResolution, PoolError};
use std::time::Duration;
use types::hash::PoolRef;
use types::pool::{DepositResolution, DepositStatus, PoolStep};
use types::{Address, EvmAddress, Timestamp};

/// What of the ledger's window a retry must leave its next drive (the review, 2026-10-24).
pub const RETRY_MARGIN: Duration = Duration::from_secs(3_600);

/// Whether a ledger still deduplicates a transfer of a payout started at `attempt_at` after
/// a retry at `now`, its next drive [`RETRY_MARGIN`] away at most: a transfer that landed is
/// then answered `Duplicate`.
pub fn deduplicated(attempt_at: Timestamp, now: Timestamp) -> bool {
    now.saturating_duration_since(attempt_at)
        .saturating_add(RETRY_MARGIN)
        < DEDUP_WINDOW
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
            if let Some(attempt) = deposit.payout.attempt {
                if !deduplicated(attempt.created_at, now) {
                    return Err(PoolError::NotAllowed(format!(
                        "the ledger no longer deduplicates its payout's transfer (memo its ref, \
                         created_at_time {}): read the ledger for it before anything",
                        attempt.created_at.as_nanos()
                    )));
                }
            }
            record(
                PoolStep::DepositResolved {
                    r#ref,
                    resolution: DepositResolution::Retry,
                    retry_at: None,
                },
                now,
            )?;
            Ok(None)
        }
        WireResolution::Refund => {
            if deposit.payout.attempt.is_some() {
                return Err(PoolError::NotAllowed(
                    "a started payout is never refunded on Base: its transfer may have \
                     landed; retry it while the ledger deduplicates it, else read the ledger"
                        .to_string(),
                ));
            }
            let (key, orphan) = deposit
                .refund_orphan()
                .ok_or_else(|| PoolError::NotAllowed("nothing arrived".to_string()))?;
            let payer = deposit.terms.src_payer;
            if payer == EvmAddress::ZERO {
                return Err(PoolError::NotAllowed(
                    "the deposit has no payer to refund".to_string(),
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
