//! Orphans held for a controller (spec 3.1, Jeff 2026-10-19: ALWAYS held, nothing
//! automatic): USDC at the canister's Base address that no deposit could take. A controller
//! refunds one to an address (`USDC.transfer(to, amount - min_network_fee)` on Base, the fee
//! kept on the Base fee line for the transfer's gas) or books it to the Base books'
//! unattributed line; each one resolved once.

#[cfg(test)]
mod tests;

use crate::state::Store;
use crate::storage::events::read_state;
use crate::tx::{self, Detail};
use alloy_primitives::U256;
use ic_stable_structures::Storable;
use settlement_api::types::pool::{evm_address, nat, OrphanAction, OrphanView, PoolError};
use std::borrow::Cow;
use types::config::GasAction;
use types::events::TxPurpose;
use types::pool::{orphan_key, CreditKey, Orphan, OrphanStatus, PoolStep};
use types::{Address, TokenAmount, Wei};

/// An orphan as `pool_orphans` lists it, its key the credit key's 48 bytes.
pub fn view(key: &CreditKey, orphan: &Orphan) -> OrphanView {
    OrphanView {
        key: key.to_bytes().to_vec(),
        amount: nat(orphan.amount),
        block: orphan.block.get(),
        payer: orphan.payer.map(|payer| payer.to_string()),
        why: orphan.why.clone(),
    }
}

/// The orphans still held, in key order.
pub fn held() -> Vec<OrphanView> {
    read_state(|state| {
        state
            .store()
            .pool_orphans()
            .iter()
            .filter(|(_, orphan)| orphan.status == OrphanStatus::Held)
            .map(|(key, orphan)| view(key, orphan))
            .collect()
    })
}

/// The credit key `bytes` spell.
pub fn key_of(bytes: &[u8]) -> Result<CreditKey, PoolError> {
    if bytes.len() != 48 {
        return Err(PoolError::NotAllowed(
            "an orphan's key is its 48 bytes".to_string(),
        ));
    }
    Ok(CreditKey::from_bytes(Cow::Borrowed(bytes)))
}

/// What a refund sends: the orphan less the least network fee, which pays its gas. None for
/// an orphan the fee takes all of (book it instead).
pub fn refund_amount(orphan: &Orphan, min_network_fee: TokenAmount) -> Option<TokenAmount> {
    orphan
        .amount
        .checked_sub(min_network_fee)
        .filter(|sent| *sent > TokenAmount::ZERO)
}

/// The guards, in order: the halt, then the controller.
pub fn checked(halted: bool, is_controller: bool) -> Result<(), PoolError> {
    if halted {
        return Err(PoolError::Halted);
    }
    if !is_controller {
        return Err(PoolError::NotAllowed(
            "only a controller resolves an orphan".to_string(),
        ));
    }
    Ok(())
}

/// `resolve_orphan`'s body.
pub async fn resolve(
    is_controller: bool,
    key: Vec<u8>,
    action: OrphanAction,
    now: types::Timestamp,
    sanctioned: impl Fn(&Address) -> bool,
) -> Result<(), PoolError> {
    checked(crate::guards::require_not_halted().is_err(), is_controller)?;
    let key = key_of(&key)?;
    let orphan = read_state(|state| state.store().pool_orphan(&key)).ok_or(PoolError::Unknown)?;
    if orphan.status != OrphanStatus::Held {
        return Err(PoolError::NotAllowed(
            "the orphan was resolved already".to_string(),
        ));
    }
    match action {
        OrphanAction::Book => crate::storage::pool::record_at(PoolStep::OrphanBooked { key }, now)
            .map(drop)
            .map_err(|error| PoolError::NotAllowed(error.to_string())),
        OrphanAction::Refund { to } => {
            let to = evm_address("to", &to)?;
            if sanctioned(&Address::from(to)) {
                return Err(PoolError::Sanctioned(
                    "the refund's address is sanctioned".to_string(),
                ));
            }
            // the refund is an outflow: the pool's guard first
            crate::pool::require_pool_open()?;
            let settings = crate::storage::pool::ledger()
                .lines()
                .settings
                .ok_or_else(|| PoolError::Closed("the payouts have no settings".to_string()))?;
            let sent = refund_amount(&orphan, settings.min_network_fee).ok_or_else(|| {
                PoolError::NotAllowed(
                    "the network fee takes all of the orphan: book it instead".to_string(),
                )
            })?;
            let config = crate::storage::config::get();
            let chain = config
                .evm_chains
                .get(key.chain)
                .ok_or_else(|| PoolError::Closed(format!("chain {} signs nothing", key.chain)))?;
            let gas_limit = chain
                .gas_limit
                .get(GasAction::PoolTransfer)
                .ok_or_else(|| {
                    PoolError::NotAllowed("the chain has no pool_transfer gas row".to_string())
                })?;
            tx::create_and_send_with(
                TxPurpose::PoolTransfer(orphan_key(&key)),
                key.chain,
                chain.usdc,
                Wei::ZERO,
                types::swapic::transfer_calldata(to, U256::from_be_bytes(sent.to_be_bytes())),
                gas_limit,
                GasAction::PoolTransfer,
                Some(Detail::PoolTransfer { key, to, sent }),
            )
            .await
            .map(drop)
            .map_err(|error| PoolError::Unavailable(error.to_string()))
        }
    }
}
