//! `resolve_stake`: an ICP stake whose pull's answer was lost is asked
//! again with its own dedup pair while the ckUSDC ledger still deduplicates it. Past that
//! window ([`DEDUP_WINDOW`]) a pull asked again could be a second one, so the payout drive
//! leaves the stake pending for good, its room taken. A controller resolves it once:
//!
//! - `Pulled { block }`: the pull landed in that ledger block. The canister reads the block
//!   on the ckUSDC ledger first and mints only on the stake's own
//!   pull: `icrc2_transfer_from` by the canister from the stake's account to its own, of the
//!   stake's amount, the stake's op id as its memo and its start as its `created_at_time`
//!   (as `pull_stake` asks it). Its shares are minted at the pool's price now, as a pull
//!   answered late is, and its ckUSDC is free on ICP. One block resolves one stake. A
//!   ledger that cannot be read now refuses, to be asked again later.
//! - `NotPulled`: it never landed: dropped, its room freed. On the controller's word: no
//!   ledger read proves a pull absent, so the controller checks the ledger has no block of
//!   it (its op id as the memo) before answering this.
//!
//! Only past the window, so the drive and the controller never both decide one stake, and
//! never while a drive holds it.

#[cfg(test)]
mod tests;

use super::{same_account as same, LedgerBlocks};
use crate::ic::LedgerTransfer;
use crate::pool::payout::{Busy, DEDUP_WINDOW};
use crate::storage::pool as pool_store;
use candid::{Nat, Principal};
use settlement_api::types::pool::{PoolError, StakeResolution as WireResolution};
use types::hash::PoolOpId;
use types::pool::{IcpAccount, PendingOp, PoolStep, Shares, StakeResolution};
use types::{Timestamp, TokenAmount};

/// The stake a controller resolves: who, from where, how much, since when.
struct Stake {
    from: IcpAccount,
    amount: TokenAmount,
    created_at: Timestamp,
    move_fee: TokenAmount,
}

/// Whether `transfer` is stake `id`'s own pull, as `pull_stake` asks it: by `canister` from
/// the stake's account to the canister's own, of its amount, its op id the memo and its
/// start the `created_at_time`. Err with what differs.
fn is_its_pull(
    transfer: &LedgerTransfer,
    id: PoolOpId,
    stake: &Stake,
    canister: Principal,
) -> Result<(), String> {
    let main = IcpAccount {
        owner: canister,
        subaccount: None,
    };
    let checks = [
        (
            same(&transfer.from, stake.from),
            "it is not from the stake's account",
        ),
        (
            same(&transfer.to, main),
            "it does not pay the canister's account",
        ),
        (
            transfer
                .spender
                .as_ref()
                .is_some_and(|spender| same(spender, main)),
            "the canister did not pull it",
        ),
        (
            transfer.amount == Nat::from(stake.amount),
            "its amount is not the stake's",
        ),
        (
            transfer.memo.as_deref() == Some(&id.into_bytes()[..]),
            "its memo is not the stake's id",
        ),
        (
            transfer.created_at_time == Some(stake.created_at.as_nanos()),
            "its created_at_time is not the stake's start",
        ),
    ];
    match checks.into_iter().find(|(holds, _)| !holds) {
        Some((_, why)) => Err(format!("the block is not the stake's pull: {why}")),
        None => Ok(()),
    }
}

/// The stake pending under `id`, or why there is none.
fn pending_stake(id: PoolOpId) -> Result<Stake, PoolError> {
    match pool_store::ledger().pending.get(&id) {
        Some(PendingOp::Stake {
            from,
            amount,
            created_at,
            move_fee,
            ..
        }) => Ok(Stake {
            from: *from,
            amount: *amount,
            created_at: *created_at,
            move_fee: *move_fee,
        }),
        Some(PendingOp::Withdraw { .. }) => Err(PoolError::NotAllowed(
            "the operation is a withdrawal, not a stake".to_string(),
        )),
        None => Err(PoolError::Unknown),
    }
}

/// `resolve_stake`'s body, the guards given (`canister`: the canister's own principal, the
/// pull's spender and payee).
pub async fn resolve(
    halted: impl Fn() -> bool,
    is_controller: bool,
    canister: Principal,
    id: PoolOpId,
    resolution: WireResolution,
    clock: impl Fn() -> Timestamp,
    blocks: &impl LedgerBlocks,
) -> Result<(), PoolError> {
    crate::pool::controller_only(halted(), is_controller, "resolves a stake")?;
    let stake = pending_stake(id)?;
    if clock().saturating_duration_since(stake.created_at) < DEDUP_WINDOW {
        return Err(PoolError::NotAllowed(
            "the canister still asks the stake's pull again: it is a controller's only past the \
             ledger's deduplication window"
                .to_string(),
        ));
    }
    let _held = Busy::take(id.into_bytes())
        .ok_or_else(|| PoolError::Unavailable("the stake is being driven now".to_string()))?;
    let resolution = match resolution {
        WireResolution::NotPulled => StakeResolution::NotPulled,
        WireResolution::Pulled { block } => {
            let ledger = pool_store::ledger();
            if ledger
                .resolved_pulls
                .as_ref()
                .is_some_and(|used| used.contains(&block))
            {
                return Err(PoolError::NotAllowed(format!(
                    "ledger block {block} resolved a stake already"
                )));
            }
            let ckusdc = ledger
                .settings
                .as_ref()
                .map(|settings| settings.ckusdc_ledger)
                .ok_or_else(|| PoolError::Closed("the pool has no settings".to_string()))?;
            let transfer = blocks
                .transfer_at(ckusdc, block)
                .await
                .map_err(|error| super::read_error(block, error))?
                .ok_or_else(|| {
                    PoolError::NotAllowed(format!("the ledger has no block {block} of a transfer"))
                })?;
            is_its_pull(&transfer, id, &stake, canister).map_err(PoolError::NotAllowed)?;
            // after the read: the halt again, the stake still pending (its hold kept every
            // other resolution and the drive out), and the price now
            if halted() {
                return Err(PoolError::Halted);
            }
            let stake = pending_stake(id)?;
            let shares = stake
                .amount
                .checked_sub(stake.move_fee)
                .and_then(|staked| pool_store::ledger().mint_now(staked))
                .filter(|shares| *shares > Shares::ZERO)
                .ok_or_else(|| PoolError::NotAllowed("the stake mints nothing".to_string()))?;
            StakeResolution::Pulled { block, shares }
        }
    };
    pool_store::record_at(PoolStep::StakeResolved { id, resolution }, clock())
        .map(drop)
        .map_err(|error| PoolError::NotAllowed(error.to_string()))
}
