//! The rebalancing's update doors (interface section 3d).

use crate::pool::rebalance;
use crate::storage::halt::is_halted;
use ic_cdk::update;
pub use settlement_api::types::pool::{
    PoolApproveOk, PoolArrivalOutcome, PoolError, RebalanceSettings,
};

/// Controller-only, refused while halted: the rebalancing's knobs, held together.
#[update]
pub fn set_rebalance_settings(settings: RebalanceSettings) -> Result<(), PoolError> {
    let caller = ic_cdk::api::caller();
    rebalance::set_settings(is_halted(), ic_cdk::api::is_controller(&caller), settings)
}

/// Anyone (the watcher), refused while halted: an ICP to Base move's USDC reached the
/// canister's Base address in `base_tx` (and, for an Eco route, the order's `src_tx` on
/// `src_chain`; an empty `src_tx` names none). The canister reads the receipts itself, proves
/// the transfer carries the move's own ref (its CCTP envelope, or the Eco source's
/// `OrderStarted`), and credits the move once.
#[update]
pub async fn note_move_arrival(
    id: u64,
    base_tx: Vec<u8>,
    src_chain: u64,
    src_tx: Vec<u8>,
) -> Result<PoolArrivalOutcome, PoolError> {
    rebalance::arrival::note(id, base_tx, src_chain, src_tx).await
}

/// Controller-only, refused while halted, once per deploy: the canister's standing USDC
/// allowance to the ck minter's deposit helper on Ethereum (flow settings' `ck_helper`), which
/// a move to ICP by Eco deposits through. An allowance in place answers `Known`.
#[update]
pub async fn pool_approve_ck_helper() -> Result<PoolApproveOk, PoolError> {
    let caller = ic_cdk::api::caller();
    crate::pool::approve::approve_ck_helper(ic_cdk::api::is_controller(&caller))
        .await
        .map(|tx_id| PoolApproveOk { tx_id })
}
