//! `withdraw_evm` (spec 3.3, the fixed interface section 3b): an EVM staker withdraws to any
//! token on any chain with ONE EIP-712 signature, which proves the staker whichever door
//! forwards it (the frontend, or the watcher's `POST /v2/withdraw`): the canister trusts the
//! signature alone. Every check runs before anything is recorded, and the burn, the exact
//! amount and the payout's place in the queue are recorded in the one message, with no await,
//! so the pool's price cannot move between them.
//!
//! The order of the checks: the signature recovers to the staker (65 bytes, `v` 27 or 28,
//! low `s`: a contract wallet is not supported); the nonce unused by the staker; the payout's
//! deadline at least five minutes away; no sanctioned party; the rebalancing share and the
//! network fee at least their floors; payout terms the canister signs for; the shares burned
//! (`ceil(usdc * S / A)`) within `max_shares` and the staker's own; the pool open.

#[cfg(test)]
mod tests;

use crate::pool::icp_swap::{check_payout_terms, MIN_DEADLINE};
use crate::relay::{recover_as, Verifier};
use crate::state::{State, Store};
use candid::Nat;
use settlement_api::types::pool::{amount, evm_address, u128_of, PoolError};
use types::pool::payout::{EvmWithdrawal, PayoutKind, Undo};
use types::pool::withdrawal::WithdrawalRequest;
use types::pool::{NonceUse, PoolAccount, PoolStep, Shares};
use types::{Address, ChainId, EvmAddress, Timestamp};

/// The wire request as the domain takes it.
pub fn request_of(
    args: settlement_api::types::pool::EvmWithdrawal,
) -> Result<WithdrawalRequest, PoolError> {
    Ok(WithdrawalRequest {
        staker: evm_address("staker", &args.staker)?,
        usdc: amount("usdc", &args.usdc)?,
        max_shares: Shares::new(u128_of("max_shares", &args.max_shares)?),
        move_fee: amount("move_fee", &args.move_fee)?,
        network_fee: amount("network_fee", &args.network_fee)?,
        payout: args.payout.try_into()?,
        nonce: args.nonce,
    })
}

/// What a withdrawal burns and the payout it opens, once every check passed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Planned {
    pub withdrawal: EvmWithdrawal,
}

/// The staker the signature recovers to, over the request's digest on the pool chain.
pub fn signer(
    request: &WithdrawalRequest,
    signature: &[u8],
    chain_id: ChainId,
    swapic: EvmAddress,
) -> Result<EvmAddress, PoolError> {
    let digest = request.digest(chain_id, swapic);
    match recover_as(Verifier::LowS, digest, signature) {
        Ok(Some(signer)) => Ok(signer),
        Ok(None) => Err(PoolError::NotAllowed(
            "the signature recovers to no address".to_string(),
        )),
        Err(error) => Err(PoolError::NotAllowed(error.to_string())),
    }
}

/// Every rule a withdrawal meets before anything is recorded, in the module's order. `closed`
/// is the pool's guard, read by the caller.
#[allow(clippy::too_many_arguments)]
pub fn plan<S: Store>(
    state: &State<S>,
    config: &types::Config,
    request: &WithdrawalRequest,
    signature: &[u8],
    now: Timestamp,
    canister: Option<EvmAddress>,
    closed: Option<String>,
    sanctioned: impl Fn(&Address) -> bool,
) -> Result<Planned, PoolError> {
    let chain_id = config
        .pool_chain
        .ok_or_else(|| PoolError::Closed("no pool chain is configured".to_string()))?;
    let swapic = config
        .evm_chains
        .get(chain_id)
        .map(|chain| chain.swapic)
        .ok_or_else(|| PoolError::Closed(format!("the pool chain {chain_id} has no Swapic")))?;
    if signer(request, signature, chain_id, swapic)? != request.staker {
        return Err(PoolError::NotAllowed(
            "the signature is not the staker's: a contract wallet's ERC-1271 signature is not \
             supported"
                .to_string(),
        ));
    }
    let used = NonceUse {
        staker: request.staker,
        nonce: request.nonce,
    };
    if state.store().is_withdrawal_nonce_used(&used) {
        return Err(PoolError::NotAllowed(format!(
            "the staker's nonce {} is used",
            request.nonce
        )));
    }
    let earliest = now
        .checked_add(MIN_DEADLINE)
        .unwrap_or(Timestamp::from_nanos(u64::MAX));
    if request.payout.deadline < earliest {
        return Err(PoolError::Expired);
    }
    if sanctioned(&Address::from(request.staker)) {
        return Err(PoolError::Sanctioned(
            "the staker is sanctioned".to_string(),
        ));
    }
    let ledger = state.pool();
    ledger
        .settings
        .as_ref()
        .ok_or_else(|| PoolError::Closed("the pool has no settings".to_string()))?;
    let payouts = ledger
        .lines()
        .settings
        .ok_or_else(|| PoolError::Closed("the payouts have no settings".to_string()))?;
    if request.network_fee < payouts.min_network_fee {
        return Err(PoolError::NotAllowed(format!(
            "the network fee must be at least {}",
            payouts.min_network_fee
        )));
    }
    check_payout_terms(config, &request.payout, canister, &sanctioned)?;
    let owner = PoolAccount::Evm(request.staker);
    let owned = state
        .store()
        .pool_share(&owner.key())
        .map_or(Shares::ZERO, |line| line.shares);
    let burned = Shares::burn_for(request.usdc, ledger.total_assets(), ledger.total_shares)
        .ok_or_else(|| PoolError::NotAllowed("the pool's assets cannot pay that".to_string()))?;
    if burned > request.max_shares || burned > owned {
        return Err(PoolError::ShortShares {
            have: Nat::from(owned.min(request.max_shares).get()),
            need: Nat::from(burned.get()),
        });
    }
    let withdrawal = EvmWithdrawal {
        staker: request.staker,
        usdc: request.usdc,
        max_shares: request.max_shares,
        move_fee: request.move_fee,
        network_fee: request.network_fee,
        nonce: request.nonce,
        burned,
    };
    withdrawal.payout_amount().ok_or_else(|| {
        PoolError::NotAllowed(
            "the rebalancing share and the network fee take all of the withdrawal".to_string(),
        )
    })?;
    if let Some(reason) = closed {
        return Err(PoolError::Closed(reason));
    }
    Ok(Planned { withdrawal })
}

/// Records the planned withdrawal: opened (its shares burned, its nonce spent), its exact
/// amount, and its place (waiting when the Base pocket covers it behind an empty queue, else
/// queued). Answers the payout's id. Every line was checked by [`plan`], so a refusal here is
/// a bug, and the message traps, rolling back every line before it.
pub fn record(planned: Planned, terms: types::pool::PayoutTerms, now: Timestamp) -> u64 {
    let withdrawal = planned.withdrawal;
    let id = crate::storage::pool::ledger().lines().next_id;
    let owner = PoolAccount::Evm(withdrawal.staker);
    crate::storage::pool::record_at(
        PoolStep::PayoutOpened {
            id,
            kind: Box::new(PayoutKind::Withdrawal(withdrawal)),
            terms,
            undo: Undo::Remint {
                owner,
                shares: withdrawal.burned,
            },
            plan: None,
        },
        now,
    )
    .expect("BUG: plan checked the withdrawal's opening");
    crate::storage::pool::record_at(
        PoolStep::PayoutAmountKnown {
            id,
            amount: withdrawal
                .payout_amount()
                .expect("BUG: plan checked the amount"),
            received: None,
        },
        now,
    )
    .expect("BUG: plan checked the withdrawal's amount");
    crate::pool::icp_swap::place(id, now).expect("BUG: a known payout waits or is queued");
    id
}

/// `withdraw_evm`'s body: the payout's id and the shares burned.
pub fn withdraw_evm(
    args: settlement_api::types::pool::EvmWithdrawal,
    signature: Vec<u8>,
    sanctioned: impl Fn(&Address) -> bool,
) -> Result<(u64, Shares), PoolError> {
    crate::pool::require_running()?;
    let request = request_of(args)?;
    let config = crate::storage::config::get();
    let canister = crate::storage::ecdsa_address::get();
    let closed = crate::pool::pool_closed();
    let now = crate::pool::now();
    let planned = crate::storage::events::read_state(|state| {
        plan(
            state,
            &config,
            &request,
            &signature,
            now,
            canister,
            closed,
            &sanctioned,
        )
    })?;
    let burned = planned.withdrawal.burned;
    Ok((record(planned, request.payout, now), burned))
}
