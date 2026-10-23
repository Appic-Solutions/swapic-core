//! `resolve_stake` (Jeff, 2026-10-24): an ICP stake whose pull's answer was lost is asked
//! again with its own dedup pair while the ckUSDC ledger still deduplicates it. Past that
//! window ([`DEDUP_WINDOW`]) a pull asked again could be a second one, so the payout drive
//! leaves the stake pending for good, its room taken. A controller, having looked at the
//! ledger, resolves it once:
//!
//! - `Pulled { block }`: the pull landed in that ledger block: its shares are minted at the
//!   pool's price now, as a pull answered late is, and its ckUSDC is free on ICP.
//! - `NotPulled`: it never landed: dropped, its room freed.
//!
//! Only past the window, so the drive and the controller never both decide one stake, and
//! never while a drive holds it.

#[cfg(test)]
mod tests;

use crate::pool::payout::{Busy, DEDUP_WINDOW};
use crate::storage::pool as pool_store;
use settlement_api::types::pool::{PoolError, StakeResolution as WireResolution};
use types::hash::PoolOpId;
use types::pool::{PendingOp, PoolStep, Shares, StakeResolution};
use types::Timestamp;

/// `resolve_stake`'s body, the guards given.
pub fn resolve(
    halted: bool,
    is_controller: bool,
    id: PoolOpId,
    resolution: WireResolution,
    now: Timestamp,
) -> Result<(), PoolError> {
    if halted {
        return Err(PoolError::Halted);
    }
    if !is_controller {
        return Err(PoolError::NotAllowed(
            "only a controller resolves a stake".to_string(),
        ));
    }
    let ledger = pool_store::ledger();
    let (amount, created_at) = match ledger.pending.get(&id) {
        Some(PendingOp::Stake {
            amount, created_at, ..
        }) => (*amount, *created_at),
        Some(PendingOp::Withdraw { .. }) => {
            return Err(PoolError::NotAllowed(
                "the operation is a withdrawal, not a stake".to_string(),
            ))
        }
        None => return Err(PoolError::Unknown),
    };
    if now.saturating_duration_since(created_at) < DEDUP_WINDOW {
        return Err(PoolError::NotAllowed(
            "the canister still asks the stake's pull again: it is a controller's only past the \
             ledger's deduplication window"
                .to_string(),
        ));
    }
    let _held = Busy::take(id.into_bytes())
        .ok_or_else(|| PoolError::Unavailable("the stake is being driven now".to_string()))?;
    let resolution = match resolution {
        WireResolution::Pulled { block } => StakeResolution::Pulled {
            block,
            shares: ledger
                .mint_now(amount)
                .filter(|shares| *shares > Shares::ZERO)
                .ok_or_else(|| PoolError::NotAllowed("the stake mints nothing".to_string()))?,
        },
        WireResolution::NotPulled => StakeResolution::NotPulled,
    };
    pool_store::record_at(PoolStep::StakeResolved { id, resolution }, now)
        .map(drop)
        .map_err(|error| PoolError::NotAllowed(error.to_string()))
}
