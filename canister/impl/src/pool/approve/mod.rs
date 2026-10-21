//! The canister's standing USDC allowance to Swapic on the pool chain (spec 3.2, Jeff
//! 2026-10-19): every pool payout is the canister's own `Swapic.run(order)`, which pulls the
//! order's USDC from its payer, the canister, so one approval of Swapic for everything serves
//! every payout with ONE signature. Safe because `run` pulls only from `payer == msg.sender`:
//! the allowance serves the canister's own calls and no one else's.
//!
//! `pool_approve_swapic`, controller-only: one `eth_call` of `allowance(canister, Swapic)`;
//! below 2^255 the canister signs `approve(Swapic, 2^256 - 1)` (purpose `PoolApprove`, its
//! gas held to the `pool_approve` row), else it answers that the allowance is in place.

#[cfg(test)]
mod tests;

use crate::ecdsa;
use crate::evm_reads::{self, Simulated};
use crate::guards::require_not_halted;
use crate::storage::{config, outbox};
use crate::tx;
use alloy_primitives::U256;
use settlement_api::types::pool::PoolError;
use types::config::GasAction;
use types::events::TxPurpose;
use types::{EvmAddress, Wei};

/// From this allowance up, the approval is in place: half of all there is, which no amount of
/// payouts spends down.
pub fn in_place(allowance: U256) -> bool {
    allowance >= U256::from(1) << 255
}

/// `allowance(owner, spender)`.
pub fn allowance_calldata(owner: EvmAddress, spender: EvmAddress) -> Vec<u8> {
    types::swapic::allowance_calldata(owner, spender)
}

/// `approve(spender, 2^256 - 1)`.
pub fn approve_calldata(spender: EvmAddress) -> Vec<u8> {
    types::swapic::approve_calldata(spender, U256::MAX)
}

/// The guards, in order: the halt, then the controller.
pub fn checked(halted: bool, is_controller: bool) -> Result<(), PoolError> {
    if halted {
        return Err(PoolError::Halted);
    }
    if !is_controller {
        return Err(PoolError::NotAllowed(
            "only a controller approves Swapic".to_string(),
        ));
    }
    Ok(())
}

/// What the allowance read answered, as an amount; None for an answer that is not one word.
pub fn allowance_of(answer: &Simulated) -> Option<U256> {
    match answer {
        Simulated::Returned(bytes) if bytes.len() == 32 => Some(U256::from_be_slice(bytes)),
        _ => None,
    }
}

/// `pool_approve_swapic`'s body: the signed approval's id in `signed_txs`. An allowance in
/// place answers `Known`; an approval still on its way answers `Unavailable`.
pub async fn approve_swapic(is_controller: bool) -> Result<u64, PoolError> {
    checked(require_not_halted().is_err(), is_controller)?;
    let config = config::get();
    let chain_id = config
        .pool_chain
        .ok_or_else(|| PoolError::Closed("no pool chain is configured".to_string()))?;
    let chain = config.evm_chains.get(chain_id).ok_or_else(|| {
        PoolError::Closed(format!(
            "the pool chain {chain_id} is not one the canister signs on"
        ))
    })?;
    let gas_limit = chain.gas_limit.get(GasAction::PoolApprove).ok_or_else(|| {
        PoolError::NotAllowed("the pool chain has no pool_approve gas row".to_string())
    })?;
    if outbox::find(TxPurpose::PoolApprove).is_some() {
        return Err(PoolError::Unavailable(
            "an approval of Swapic is on its way".to_string(),
        ));
    }
    let canister = ecdsa::canister_address()
        .await
        .map_err(|error| PoolError::Unavailable(error.to_string()))?;
    let answer = evm_reads::eth_call(
        chain_id,
        canister,
        chain.usdc,
        &allowance_calldata(canister, chain.swapic),
    )
    .await
    // the kind only: an outcall's text can carry the provider's url and key
    .map_err(|error| PoolError::Unavailable(format!("the allowance read: {:?}", error.kind())))?;
    let allowance = allowance_of(&answer).ok_or_else(|| {
        PoolError::Unavailable("the allowance read answered no amount".to_string())
    })?;
    if in_place(allowance) {
        return Err(PoolError::Known);
    }
    // the halt and another approval can land while the read was out
    checked(require_not_halted().is_err(), is_controller)?;
    if outbox::find(TxPurpose::PoolApprove).is_some() {
        return Err(PoolError::Unavailable(
            "an approval of Swapic is on its way".to_string(),
        ));
    }
    let signed = tx::create_and_send(
        TxPurpose::PoolApprove,
        chain_id,
        chain.usdc,
        Wei::ZERO,
        approve_calldata(chain.swapic),
        gas_limit,
        GasAction::PoolApprove,
    )
    .await
    .map_err(|error| PoolError::Unavailable(error.to_string()))?;
    Ok(signed.id.get())
}
