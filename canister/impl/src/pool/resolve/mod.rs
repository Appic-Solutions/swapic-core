//! `resolve_payout` (Jeff, 2026-10-23, the review's code-review finding): a payout stopped for
//! a human (`PayoutStopped`, status `Failed`) held its lines for good, since nothing led out
//! of `Failed`. A controller, having looked at the ledgers and the chain, resolves it once:
//!
//! - `Refund`: back on its undo. An ICP swap's refund (all its ckUSDC less one ledger fee, or
//!   its token, `icp_swap::refund_of`) is asked next by the payout drive, under the pool's
//!   guard like every outflow, with the undo's own dedup instant while the ledger still
//!   deduplicates it (a refund whose answer was lost, and that went out, is then the same
//!   transfer), else now; a withdrawal's shares are minted back in the same call.
//! - `MarkPaid`: its signed order ran after all (a reorg turned its revert into a payment):
//!   the paid line, once.
//!
//! A deposit's payout stopped on ICP (`DepositStatus::PayoutFailed`) and an ICP stake whose
//! pull can no longer be asked are not payouts from the Base pocket, keyed by a ref and an op
//! id: this door does not take them.

#[cfg(test)]
mod tests;

use crate::pool::payout::DEDUP_WINDOW;
use crate::storage::pool as pool_store;
use settlement_api::types::pool::{PayoutResolution as WireResolution, PoolError};
use types::pool::payout::{EvmPayout, PayoutResolution, PayoutStatus};
use types::pool::PoolStep;
use types::Timestamp;

/// The dedup instant of a refund a controller asks for: the payout's undo's own while a
/// ledger still deduplicates it, else `now`.
pub fn refund_at(payout: &EvmPayout, now: Timestamp) -> Timestamp {
    match payout.undoing_at {
        Some(at) if now.saturating_duration_since(at) < DEDUP_WINDOW => at,
        _ => now,
    }
}

fn record(step: PoolStep, now: Timestamp) -> Result<(), PoolError> {
    pool_store::record_at(step, now)
        .map(drop)
        .map_err(|error| PoolError::NotAllowed(error.to_string()))
}

/// `resolve_payout`'s body, the guards given.
pub fn resolve(
    halted: bool,
    is_controller: bool,
    id: u64,
    resolution: WireResolution,
    now: Timestamp,
) -> Result<(), PoolError> {
    if halted {
        return Err(PoolError::Halted);
    }
    if !is_controller {
        return Err(PoolError::NotAllowed(
            "only a controller resolves a payout".to_string(),
        ));
    }
    let payout = pool_store::payout(id).ok_or(PoolError::Unknown)?;
    if !matches!(payout.status, PayoutStatus::Failed(_)) {
        return Err(PoolError::NotAllowed(format!(
            "the payout is {:?}, not stopped for a human",
            payout.status
        )));
    }
    let (resolution, refund_at) = match resolution {
        WireResolution::Refund => (PayoutResolution::Refund, Some(refund_at(&payout, now))),
        WireResolution::MarkPaid => (PayoutResolution::MarkPaid, None),
    };
    record(
        PoolStep::PayoutResolved {
            id,
            resolution,
            refund_at,
        },
        now,
    )?;
    // a withdrawal's shares come back at once: no ledger is asked (the pass would do it too)
    if resolution == PayoutResolution::Refund && !payout.is_icp_swap() {
        record(
            PoolStep::PayoutUndone {
                id,
                why: types::pool::payout::REFUNDED.to_string(),
                block: None,
            },
            now,
        )?;
    }
    Ok(())
}
