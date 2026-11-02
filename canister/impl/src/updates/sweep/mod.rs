//! The controller's sweeps of what is the canister's own: never pool money, never more than
//! the books allow, one log line each.

use ic_cdk::update;
pub use settlement_api::types::sweep::{
    EvmSweepAsset, IcpSweepAsset, SweepError, SweepEvmArgs, SweepEvmOk, SweepIcpArgs, SweepIcpOk,
};

/// Controller-only, refused while halted. Sends `amount` of the canister's own `asset` on
/// `chain_id` to `to`, signed from the canister's address with the chain's `sweep` gas row
/// at a gas price the canister reads itself:
///
/// - `Native`: the chain's gas coin, refused while any transaction of the canister on that
///   chain is still open (it may need the gas for a fee bump).
/// - `FeeUsdc`: `USDC.transfer` of booked fee income only: at most the chain books' fee
///   income, plus the pool's Base fee line on the pool chain. The amount leaves the books
///   as the transaction is signed and comes back if it reverts or is cancelled.
///
/// Refused for the canister's own address, the zero address and a sanctioned one. Answers
/// the sweep's id and its transaction's id in `signed_txs`.
#[update]
pub async fn sweep_evm(args: SweepEvmArgs) -> Result<SweepEvmOk, SweepError> {
    crate::sweep::evm::sweep(args).await
}

/// Controller-only, refused while halted. Sends `amount` of the canister's own `asset` on
/// ICP to `to`, the ledger's fee paid on top from the same account:
///
/// - `CkUsdcFees`: at most the pool's ICP fee line, never the ICP pocket's ckUSDC.
/// - `CkEth`: refused while a rebalancing move is open (its minter withdrawal spends
///   ckETH).
/// - `Icp`: refused while an ICP swap or withdrawal is open (a route through ICP holds
///   the user's ICP between its legs).
///
/// The amount leaves the books before the transfer is asked and comes back if the ledger
/// refuses it. Refused for the canister's own account. Answers the sweep's id and the
/// ledger block of its transfer.
#[update]
pub async fn sweep_icp(args: SweepIcpArgs) -> Result<SweepIcpOk, SweepError> {
    crate::sweep::icp::sweep(args).await
}
