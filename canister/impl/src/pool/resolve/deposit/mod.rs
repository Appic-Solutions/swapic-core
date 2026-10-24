//! `resolve_deposit` (Jeff, 2026-10-24): an EVM to ICP deposit whose payout is held for a
//! human (`DepositStatus::PayoutFailed`): a started payout after its tenth failure, a queued
//! one that never started (CR-08), a stake whose mint kept failing. Its lines stayed held,
//! since nothing led out of it. A controller resolves it once (the rules of the review,
//! 2026-10-24):
//!
//! - `Retry`: back to its payout with fresh attempts, keyed on its transfer. A started one
//!   whose transfer cannot land twice (never asked, or refused by the ledger for sure at its
//!   last ask with no answer of it ever lost: after a lost answer, a ledger that does not
//!   deduplicate may refuse a re-ask because the first drained the money) is asked afresh at
//!   the retry's instant, whatever token it pays, at any time. One that may have landed (its
//!   last ask not refused, or an answer of it once lost) pays again with its own dedup pair,
//!   so the ledger answers `Duplicate` and it is recorded paid once: only on the ckUSDC
//!   ledger the payout started with, which deduplicates it (another token's ledger may take
//!   the old pair as a new transfer), and only with [`RETRY_MARGIN`] of the ledger's window
//!   left; else `MarkPaid`. One that never started is queued again, at the back; a stake
//!   mints again. The `Retry` lines d979af1..026e392 wrote (no instant) never ran anywhere
//!   (never deployed).
//! - `Refund`: only when the canister knows nothing of its payout went out: never started;
//!   or started, no ICPSwap leg moved money, and its transfer to the recipient never asked or
//!   refused by the ledger for sure at its last ask with no answer of it ever lost
//!   (`PayoutProgress::nothing_went_out`, the fold's rule too). What arrived goes back to its
//!   payer on Base, less the least network fee, through the orphan refund path, behind the
//!   pool's guard: its arrival is an orphan held under its transfer's key, and the canister
//!   signs `USDC.transfer(payer, amount - fee)`. A signature that fails leaves the orphan
//!   held, for `resolve_orphan`. A deposit with no payer is not refunded.
//! - `MarkPaid { block }`: its transfer landed in `block`. The canister reads the block on
//!   the transfer's own ledger and books the deposit paid, once, only on exactly the transfer
//!   its payout asked: from the canister's own account to the recipient, of what it sent,
//!   its memo and `created_at_time` its dedup pair. Any time, so a payout past the window
//!   whose transfer landed is never stuck. A ledger not read now refuses, to be asked later.
//!
//! A started payout past the window whose transfer's outcome is unknown and whose block the
//! controller has not found is refused every way, the answer naming what to search the
//! ledger for.

#[cfg(test)]
mod tests;

use super::{same_account, LedgerBlocks};
use crate::ic::LedgerTransfer;
use crate::pool::orphans::{self, Refund};
use crate::pool::payout::Busy;
#[cfg(test)]
use crate::pool::payout::DEDUP_WINDOW;
use crate::storage::pool as pool_store;
use candid::{Nat, Principal};
use settlement_api::types::pool::{DepositResolution as WireResolution, PoolError};
use types::hash::PoolRef;
use types::pool::{DepositResolution, DepositStatus, IcpAccount, PayoutAttempt, PoolStep, Sent};
use types::{Address, EvmAddress, Timestamp};

/// The retry's margin and the window, the fold's own rule too.
pub use types::pool::{deduplicated, RETRY_MARGIN};

fn record(step: PoolStep, now: Timestamp) -> Result<(), PoolError> {
    pool_store::record_at(step, now)
        .map(drop)
        .map_err(|error| PoolError::NotAllowed(error.to_string()))
}

/// What a controller searches `ledger` for to find whether a started payout's transfer
/// landed: to its recipient, its memo and `created_at_time` its dedup pair.
fn search(ledger: Principal, to: IcpAccount, attempt: &PayoutAttempt) -> String {
    format!(
        "search ledger {ledger} for its transfer to {} (subaccount {}) with memo {} and \
         created_at_time {}; MarkPaid with its block if it landed",
        to.owner,
        to.subaccount
            .map_or("none".to_string(), |bytes| crate::rpc::hex0x(&bytes)),
        crate::rpc::hex0x(&attempt.memo),
        attempt.created_at.as_nanos()
    )
}

/// Whether `transfer` is exactly the transfer a payout asked (`sent`, its dedup pair in
/// `attempt`): from `canister`'s own account, no spender, to `to`. Err with what differs.
fn is_its_transfer(
    transfer: &LedgerTransfer,
    to: IcpAccount,
    sent: &Sent,
    attempt: &PayoutAttempt,
    canister: Principal,
) -> Result<(), String> {
    let main = IcpAccount {
        owner: canister,
        subaccount: None,
    };
    let checks = [
        (
            same_account(&transfer.from, main),
            "it is not from the canister's account",
        ),
        (transfer.spender.is_none(), "it is a transfer_from"),
        (
            same_account(&transfer.to, to),
            "it does not pay the recipient",
        ),
        (
            transfer.amount == Nat::from(sent.amount),
            "its amount is not what the payout sent",
        ),
        (
            transfer.memo.as_deref() == Some(&attempt.memo[..]),
            "its memo is not the payout's",
        ),
        (
            transfer.created_at_time == Some(attempt.created_at.as_nanos()),
            "its created_at_time is not the payout's",
        ),
    ];
    match checks.into_iter().find(|(holds, _)| !holds) {
        Some((_, why)) => Err(format!("the block is not the payout's transfer: {why}")),
        None => Ok(()),
    }
}

/// `resolve_deposit`'s body, the guards given (`open`: the pool's guard; `canister`: the
/// canister's own principal, a payout's payer): for a refund, the transfer to sign, every
/// check of it made before its line.
#[allow(clippy::too_many_arguments)]
pub async fn resolve(
    halted: impl Fn() -> bool,
    is_controller: bool,
    canister: Principal,
    r#ref: PoolRef,
    resolution: WireResolution,
    clock: impl Fn() -> Timestamp,
    sanctioned: impl Fn(&Address) -> bool,
    open: impl Fn() -> Result<(), PoolError>,
    blocks: &impl LedgerBlocks,
) -> Result<Option<Refund>, PoolError> {
    if halted() {
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
    let ckusdc = pool_store::ledger()
        .settings
        .map(|settings| settings.ckusdc_ledger)
        .ok_or_else(|| PoolError::Closed("the pool has no settings".to_string()))?;
    let out = deposit.out_ledger(ckusdc).unwrap_or(ckusdc);
    // its payout's dedup pair and recipient, once it started
    let started = deposit
        .payout
        .attempt
        .zip(crate::pool::payout::of_deposit(r#ref, &deposit).map(|payout| payout.to));
    let now = clock();
    match resolution {
        WireResolution::Retry => {
            let retry_at = match (&started, deposit.payout.sent) {
                // never started: queued again, nothing to ask afresh
                (None, _) => None,
                // its last ask may have landed: only with its own dedup pair, on the ckUSDC
                // ledger the payout started with, while that ledger deduplicates it
                (Some((attempt, to)), Some(sent)) if !deposit.payout.cannot_land_twice() => {
                    let search = search(out, *to, attempt);
                    let started_on = attempt.token.map_or(ckusdc, |token| token.0);
                    if sent.token.0 != started_on {
                        return Err(PoolError::NotAllowed(format!(
                            "its last transfer may have landed on ledger {}, which the canister \
                             does not know to deduplicate it: it is never retried with its old \
                             dedup pair; {search}",
                            sent.token.0
                        )));
                    }
                    if !deduplicated(attempt.created_at, now) {
                        return Err(PoolError::NotAllowed(format!(
                            "the ledger no longer deduplicates its payout's transfer: {search}"
                        )));
                    }
                    None
                }
                // nothing of its transfer can land twice (never asked, or refused for sure
                // with no answer of it ever lost): asked afresh, whatever it pays, any time
                (Some(_), _) => Some(now),
            };
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
            if !deposit.payout.nothing_went_out() {
                let search =
                    started.map_or(String::new(), |(attempt, to)| search(out, to, &attempt));
                return Err(PoolError::NotAllowed(format!(
                    "its payout may have sent something (a transfer whose outcome is not known, \
                     or an ICPSwap leg that moved money): it is not refunded on Base; {search}"
                )));
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
        WireResolution::MarkPaid { block } => {
            let (attempt, to) = started.ok_or_else(|| {
                PoolError::NotAllowed("its payout never started: nothing was paid".to_string())
            })?;
            let sent = deposit.payout.sent.ok_or_else(|| {
                PoolError::NotAllowed(
                    "its payout never asked its transfer: nothing was paid".to_string(),
                )
            })?;
            let transfer = blocks
                .transfer_at(sent.token.0, block)
                .await
                .map_err(|error| super::read_error(block, error))?
                .ok_or_else(|| {
                    PoolError::NotAllowed(format!("the ledger has no block {block} of a transfer"))
                })?;
            is_its_transfer(&transfer, to, &sent, &attempt, canister)
                .map_err(PoolError::NotAllowed)?;
            // after the read: the halt again, and still held (its hold kept every other
            // resolution and the drive out)
            if halted() {
                return Err(PoolError::Halted);
            }
            if !pool_store::deposit(&r#ref)
                .is_some_and(|deposit| matches!(deposit.status, DepositStatus::PayoutFailed(_)))
            {
                return Err(PoolError::NotAllowed(
                    "the deposit is no longer held for a human".to_string(),
                ));
            }
            record(
                PoolStep::DepositResolved {
                    r#ref,
                    resolution: DepositResolution::MarkPaid { block },
                    retry_at: None,
                },
                clock(),
            )?;
            Ok(None)
        }
    }
}
