//! `resolve_payout`: a payout stopped for
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
//! id: [`deposit`] (`resolve_deposit`) and [`stake`] (`resolve_stake`) resolve them.

pub mod deposit;
pub mod stake;
#[cfg(test)]
mod tests;

use crate::ic::{CallError, LedgerTransfer, Party};
use crate::pool::payout::DEDUP_WINDOW;
use crate::storage::pool as pool_store;
use candid::Principal;
use settlement_api::types::pool::{PayoutResolution as WireResolution, PoolError};
use types::pool::payout::{EvmPayout, PayoutResolution, PayoutStatus};
use types::pool::{IcpAccount, PoolStep};
use types::Timestamp;

/// What a controller's resolution reads of an ICRC ledger, so a door runs the same against a
/// fake.
#[allow(async_fn_in_trait)]
pub trait LedgerBlocks {
    /// The transfer in `ledger`'s block `block`: none for no such block, or one that is no
    /// transfer.
    async fn transfer_at(
        &self,
        ledger: Principal,
        block: u64,
    ) -> Result<Option<LedgerTransfer>, CallError>;
}

impl LedgerBlocks for crate::pool::payout::Live {
    async fn transfer_at(
        &self,
        ledger: Principal,
        block: u64,
    ) -> Result<Option<LedgerTransfer>, CallError> {
        let configured = crate::storage::pool::ledger()
            .settings
            .and_then(|settings| settings.routes)
            .and_then(|routes| routes.icp_ledger)
            .map(|icp| icp.0);
        let is_icp = crate::ic::is_icp_ledger(ledger, configured);
        crate::ic::ledger_transfer_at(ledger, block, is_icp).await
    }
}

/// Whether the party a ledger block names is `expected`: an ICRC account, the default
/// subaccount written either way, or the ICP ledger's account identifier of it.
pub fn same_account(party: &Party, expected: IcpAccount) -> bool {
    match party {
        Party::Account(account) => {
            let theirs = match account.subaccount.as_deref() {
                None => Some([0; 32]),
                Some(bytes) => <[u8; 32]>::try_from(bytes).ok(),
            };
            account.owner == expected.owner && theirs == Some(expected.subaccount_bytes())
        }
        Party::Identifier(identifier) => *identifier == expected.identifier(),
    }
}

/// A ledger block's read that failed, in the interface's words: a refusal stays one; a
/// ledger not read now (none of its reads answering, a stopped ledger) is asked again later.
pub fn read_error(block: u64, error: CallError) -> PoolError {
    match error {
        CallError::Refused(why) => PoolError::NotAllowed(why),
        error => PoolError::Unavailable(format!(
            "the ledger's block {block} was not read, try later: {error}"
        )),
    }
}

/// The dedup instant of a refund a controller asks for: the payout's undo's own while a
/// ledger still deduplicates it, else `now`.
pub fn refund_at(payout: &EvmPayout, now: Timestamp) -> Timestamp {
    match payout.undoing_at {
        Some(at) if now.saturating_duration_since(at) < DEDUP_WINDOW => at,
        _ => now,
    }
}

/// Records `step` at `now`; a line the fold refuses is the door's refusal.
pub(crate) fn record(step: PoolStep, now: Timestamp) -> Result<(), PoolError> {
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
    crate::pool::controller_only(halted, is_controller, "resolves a payout")?;
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
