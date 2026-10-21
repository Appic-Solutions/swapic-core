//! The rebalancing's update doors (interface section 3d).

use crate::pool::rebalance;
use crate::storage::halt::is_halted;
use ic_cdk::update;
pub use settlement_api::types::pool::{
    MoveResolution, PoolApproveOk, PoolArrivalOutcome, PoolError, RebalanceSettings,
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
/// `OrderStarted`), and credits the move once. A caller other than the watcher is held to 10
/// reports a minute in total and 3 an hour per `base_tx` (P11e), refused `Unavailable` before
/// any outcall.
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

/// Watcher-only, refused while halted and while the pool is closed: an order (`abi.encode` of
/// an Order) for move `id`, signed with `gas_limit` if its route is taken. Checked as
/// untrusted input, every rule of the spec's section 3.4; taken only within the move's window
/// and when cheaper than any taken before: the cheapest at the window's end wins.
#[update]
pub async fn propose_rebalance(id: u64, order: Vec<u8>, gas_limit: u64) -> Result<(), PoolError> {
    rebalance::propose::propose(id, order, gas_limit).await
}

/// Controller-only, refused while halted: a stuck move sent on from where it stands
/// (`Retry`: a reverted order signed again, the same one; a minter's call a controller found
/// never ran asked again; a wait read again), or written off with why (`Lost`).
#[update]
pub fn resolve_move(id: u64, resolution: MoveResolution) -> Result<(), PoolError> {
    let caller = ic_cdk::api::caller();
    rebalance::resolve(
        is_halted(),
        ic_cdk::api::is_controller(&caller),
        id,
        resolution,
        crate::pool::now(),
    )
}
