//! Orphans held for a controller (spec 3.1, Jeff 2026-10-19: ALWAYS held, nothing
//! automatic): USDC at the canister's Base address that no deposit could take. A controller
//! refunds one to an address (`USDC.transfer(to, amount - min_network_fee)` on Base, the fee
//! kept on the Base fee line for the transfer's gas) or books it to the Base books'
//! unattributed line; each one resolved once. A deposit's transfer found without its terms
//! (P11d) awaits them for 24 hours first, and is a controller's only after that.

#[cfg(test)]
mod tests;

use crate::state::Store;
use crate::storage::events::read_state;
use crate::tx::{self, Detail};
use ic_stable_structures::Storable;
use settlement_api::types::pool::{evm_address, nat, OrphanAction, OrphanView, PoolError};
use std::borrow::Cow;
use types::config::GasAction;
use types::events::TxPurpose;
use types::pool::{orphan_key, CreditKey, Orphan, PoolStep};
use types::{Address, EvmAddress, GasAmount, TokenAmount, Wei};

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

/// The orphans a controller may resolve at `now`, in key order: held, or past their wait
/// for terms.
pub fn held(now: types::Timestamp) -> Vec<OrphanView> {
    read_state(|state| {
        state
            .store()
            .pool_orphans()
            .iter()
            .filter(|(_, orphan)| orphan.status.resolvable(now))
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
    crate::pool::controller_only(halted, is_controller, "resolves an orphan")
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
    // a refund is a transaction no call brings a reading for: the gas is read first, so the
    // checks below see the orphan as it is after the read
    if matches!(action, OrphanAction::Refund { .. }) {
        crate::gas::ensure_fresh(key.chain)
            .await
            .map_err(|failure| PoolError::Unavailable(format!("the gas price: {failure:?}")))?;
        checked(crate::guards::require_not_halted().is_err(), is_controller)?;
    }
    let orphan = read_state(|state| state.store().pool_orphan(&key)).ok_or(PoolError::Unknown)?;
    if !orphan.status.resolvable(now) {
        return Err(PoolError::NotAllowed(
            "the orphan was resolved already, or still awaits its terms".to_string(),
        ));
    }
    match action {
        OrphanAction::Book => crate::storage::pool::record_at(PoolStep::OrphanBooked { key }, now)
            .map(drop)
            .map_err(|error| PoolError::NotAllowed(error.to_string())),
        OrphanAction::Refund { to } => {
            let to = evm_address("to", &to)?;
            send(refund_call(
                key,
                &orphan,
                to,
                sanctioned,
                crate::pool::require_pool_open,
            )?)
            .await
        }
    }
}

/// An orphan's refund with every check made: what [`send`] signs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refund {
    pub key: CreditKey,
    pub to: EvmAddress,
    /// The orphan less the least network fee, which pays the transfer's gas.
    pub sent: TokenAmount,
    /// The chain's USDC, the transfer's target.
    pub usdc: EvmAddress,
    pub gas_limit: GasAmount,
}

/// The refund of `orphan` under `key` to `to`, every check made before anything is recorded:
/// the address not sanctioned, the pool's guard (`open`: the refund is an outflow), the
/// network fee leaving something of it, the chain signing and its gas row.
pub fn refund_call(
    key: CreditKey,
    orphan: &Orphan,
    to: EvmAddress,
    sanctioned: impl Fn(&Address) -> bool,
    open: impl Fn() -> Result<(), PoolError>,
) -> Result<Refund, PoolError> {
    if sanctioned(&Address::from(to)) {
        return Err(PoolError::Sanctioned(
            "the refund's address is sanctioned".to_string(),
        ));
    }
    open()?;
    let settings = crate::storage::pool::ledger()
        .lines()
        .settings
        .ok_or_else(|| PoolError::Closed("the payouts have no settings".to_string()))?;
    let sent = refund_amount(orphan, settings.min_network_fee).ok_or_else(|| {
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
    Ok(Refund {
        key,
        to,
        sent,
        usdc: chain.usdc,
        gas_limit,
    })
}

/// Signs `refund`: `USDC.transfer(to, sent)` on its chain, recorded `OrphanRefundSigned`.
pub async fn send(refund: Refund) -> Result<(), PoolError> {
    let Refund {
        key,
        to,
        sent,
        usdc,
        gas_limit,
    } = refund;
    tx::create_and_send_with(
        TxPurpose::PoolTransfer(orphan_key(&key)),
        key.chain,
        usdc,
        Wei::ZERO,
        types::swapic::transfer_calldata(to, sent.to_u256()),
        gas_limit,
        GasAction::PoolTransfer,
        Some(Detail::PoolTransfer { key, to, sent }),
    )
    .await
    .map(drop)
    .map_err(|error| PoolError::Unavailable(error.to_string()))
}
