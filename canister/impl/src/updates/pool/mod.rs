//! The pool's update doors (interface section 3).

use crate::pool;
use crate::storage::halt::is_halted;
use crate::storage::sanctions::is_sanctioned;
use ic_cdk::update;
pub use settlement_api::types::pool::{
    DepositResolution, EvmWithdrawal, IcpRoute, IcpSwapTerms, OrphanAction, PayPayoutOk,
    PayoutResolution, PayoutSettings, PoolApproveOk, PoolArrival, PoolArrivalOutcome, PoolError,
    PoolSettings, StakeIcpArgs, StakeIcpOk, StakeResolution, SwapFromIcpOk, WithdrawEvmOk,
    WithdrawIcpArgs, WithdrawIcpOk,
};
use types::pool::PoolStep;

/// Controller-only, refused while halted. Sets the pool's knobs after reading the ckUSDC
/// ledger's fee and every listed token's (`icrc1_fee`), which the ICP fees' floor at an
/// arrival reads.
#[update]
pub async fn set_pool_settings(settings: PoolSettings) -> Result<(), PoolError> {
    let caller = ic_cdk::api::caller();
    let settings =
        pool::settings::checked(is_halted(), ic_cdk::api::is_controller(&caller), settings)?;
    let settings = pool::settings::with_fees(settings).await?;
    pool::require_running()?;
    crate::storage::pool::record(PoolStep::SettingsSet { settings })
        .map_err(|error| PoolError::BadSettings(error.to_string()))?;
    crate::task_manager::restart_pool_arrivals_timer();
    // the ICP pocket's first check now, so the pool's guard does not wait a whole interval
    let tolerance = crate::storage::config::get().books_tolerance.0;
    ic_cdk::spawn(pool::icp::check_icp_pocket(tolerance));
    Ok(())
}

/// Any caller (the watcher, or the user's app as a backup), refused while halted (P11d,
/// 2026-10-21: no registration). A USDC transfer to the canister's Base address in
/// `base_tx` (and, for an Eco arrival, the order's `src_tx` on `src_chain`; an empty
/// `src_tx` names none) pays the deposit whose `terms` are handed over. The canister reads
/// the receipt itself (one batched outcall, and one more on the source chain for Eco) at
/// the crediting depth, proves every transfer to it against its rail, recomputes the ref
/// from the terms and credits the transfer whose proof carries it, at most once:
/// `Credited` (the payout follows on the canister's timer), or `Unattributed` with why,
/// held for a controller. `BadArrival` when no transfer carries the terms' ref. The
/// canister trusts only the chain and the fingerprint. A caller other than the watcher is
/// held to 10 reports a minute in total and 3 an hour per `base_tx` (P11e), refused
/// `Unavailable` before any outcall.
#[update]
pub async fn note_pool_arrival(arrival: PoolArrival) -> Result<PoolArrivalOutcome, PoolError> {
    pool::arrival::note(arrival).await
}

/// Any caller, refused while halted, while the pool is closed, for a sanctioned party, and past
/// five calls a minute or thirty an hour (this door and `swap_from_icp` together).
/// Stakes `amount` of the caller's own ckUSDC (from `from_subaccount`), pulled with
/// `icrc2_transfer_from` on the caller's approval of the canister (its main account; the
/// approval covers `amount` and the ledger's fee), for `owner`, within the TVL cap; mints
/// its shares at the pool's price before it. A pull whose answer was lost answers
/// `Unavailable`, and the canister asks again with the same pull.
#[update]
pub async fn stake_icp(args: StakeIcpArgs) -> Result<StakeIcpOk, PoolError> {
    pool::icp::stake(args, is_sanctioned).await
}

/// The ICP owner of the shares (the caller, at its subaccount `from`), refused while halted,
/// while the pool is closed, and for a sanctioned party. Burns the shares `usdc` is worth
/// (rounded up, at most `max_shares`) and pays `to` all of it as `token_out` (ckUSDC, or a
/// listed token through its ICPSwap pool, at least `min_out`; below it, all of it as ckUSDC),
/// every ledger fee out of `usdc`. Refused when the ICP pocket's free ckUSDC is short (the
/// queue is P11b). A payout that does not finish in this call answers `Unavailable` with
/// the shares burned, and the canister's timer finishes it.
#[update]
pub async fn withdraw_icp(args: WithdrawIcpArgs) -> Result<WithdrawIcpOk, PoolError> {
    pool::icp::withdraw(args, is_sanctioned).await
}

/// Controller-only, refused while halted. Sets the Base payouts' knobs: the least network
/// fee, the rebalancing share's floor, how long a waiting payout waits for an order, the
/// most gas a payout order is signed with, and the least network fee of a CCTP payout.
#[update]
pub fn set_payout_settings(settings: PayoutSettings) -> Result<(), PoolError> {
    let caller = ic_cdk::api::caller();
    let settings =
        pool::settings::payout_checked(is_halted(), ic_cdk::api::is_controller(&caller), settings)?;
    crate::storage::pool::record(PoolStep::PayoutSettingsSet { settings })
        .map_err(|error| PoolError::BadSettings(error.to_string()))?;
    Ok(())
}

/// Controller-only, refused while halted. Sets the ICP routes (interface section 3c): the
/// ICP/ckUSDC pool, and each listed token's route, directly through its ckUSDC pool or
/// through ICP. Reads the ICP ledger off the ICP/ckUSDC pool and every route ledger's fee.
/// No token has a size cap of its own: the pool's per swap cap applies.
#[update]
pub async fn set_icp_routes(
    icp_ckusdc_pool: Option<candid::Principal>,
    routes: Vec<IcpRoute>,
) -> Result<(), PoolError> {
    let caller = ic_cdk::api::caller();
    let routes = pool::settings::routes_checked(
        is_halted(),
        ic_cdk::api::is_controller(&caller),
        icp_ckusdc_pool,
        routes,
    )?;
    let ckusdc = crate::storage::pool::ledger()
        .settings
        .map(|settings| settings.ckusdc_ledger)
        .ok_or_else(|| PoolError::Closed("the pool has no settings".to_string()))?;
    let routes = pool::settings::routes_with_reads(routes, ckusdc).await?;
    pool::require_running()?;
    crate::storage::pool::record(PoolStep::IcpRoutesSet { routes })
        .map_err(|error| PoolError::BadSettings(error.to_string()))?;
    Ok(())
}

/// Controller-only, refused while halted, once per deploy and chain (Base, and Ethereum for
/// the moves to Base, P11c): reads the canister's USDC allowance to Swapic on `chain_id` and,
/// below 2^255, signs `USDC.approve(Swapic, 2^256 - 1)`, so every pool payout and move is one
/// signature of `Swapic.run`. Safe because `run` pulls only from `payer == msg.sender`. An
/// allowance already in place answers `Known`; an approval on its way answers `Unavailable`.
#[update]
pub async fn pool_approve_swapic(chain_id: u64) -> Result<PoolApproveOk, PoolError> {
    let caller = ic_cdk::api::caller();
    pool::approve::approve_swapic(
        ic_cdk::api::is_controller(&caller),
        types::ChainId::new(chain_id),
    )
    .await
    .map(|tx_id| PoolApproveOk { tx_id })
}

/// Any caller, refused while halted, while the pool is closed, for a sanctioned party, and
/// past five calls a minute or thirty an hour (this door and `stake_icp` together): an
/// ICP user swaps `amount_in` of ckUSDC or a listed ICP token (pulled with
/// `icrc2_transfer_from` on the caller's approval of the canister) to any token on an EVM
/// chain, with the quoter's payout plan (the order for the payout it expects, its gas) and
/// one slippage of at most 500 bps; the recipient, token, chain and least out are the plan's.
/// Opened only while the Base pocket can pay it. The token reaches ckUSDC through its route
/// (at least the sale floor, half the slippage below what the plan needs, or it is refunded
/// to `refund_to`); the payout from the Base pocket is the ckUSDC less the pool's premium on
/// it and the flat network fee, paid with an order built at that exact amount. Answers the
/// payout's id and where it stands; a pull the ledger refused answers its refusal.
#[update]
pub async fn swap_from_icp(terms: IcpSwapTerms) -> Result<SwapFromIcpOk, PoolError> {
    pool::icp_swap::swap_from_icp(terms, is_sanctioned).await
}

/// Controller-only, refused while halted (Jeff, 2026-10-23): a payout stopped for a human
/// (`Failed`), resolved once. `Refund`: back on its undo, an ICP swap's refund asked next (all
/// its ckUSDC less one ledger fee, or its token), a withdrawal's shares minted back at once;
/// the controller checked no refund went out before. `MarkPaid`: its signed order ran after
/// all, and its paid line is booked.
#[update]
pub fn resolve_payout(id: u64, resolution: PayoutResolution) -> Result<(), PoolError> {
    let caller = ic_cdk::api::caller();
    pool::resolve::resolve(
        is_halted(),
        ic_cdk::api::is_controller(&caller),
        id,
        resolution,
        pool::now(),
    )
}

/// Controller-only, refused while halted (Jeff, 2026-10-24): an EVM to ICP deposit held for a
/// human (`PayoutFailed`: a payout stopped after its tenth failure, a queued one that never
/// started, a stake whose mint kept failing), resolved once by its ref (its 32 bytes).
/// `Retry`: back to its payout with fresh attempts. A started one whose transfer cannot land
/// twice (never asked, or refused by the ledger for sure) is asked afresh, whatever it pays,
/// at any time; one whose last transfer may have landed keeps its own dedup pair, so a
/// transfer that landed is answered `Duplicate` and recorded paid once: only on the ckUSDC
/// ledger it started with, and with an hour of the ledger's deduplication (23 hours) left. `Refund`: only when the canister knows nothing of its payout went out (never
/// started, or no ICPSwap leg moved money and its transfer never asked or refused by the
/// ledger for sure), refused while the pool is closed and for a sanctioned or zero payer;
/// what arrived is held as an orphan under its transfer's key and the canister signs
/// `USDC.transfer(payer, amount - min_network_fee)` on Base, the fee kept for its gas. A
/// signature that fails answers `Unavailable`, the orphan held for `resolve_orphan`.
/// `MarkPaid { block }`: its transfer landed in that block; the canister reads it on the
/// transfer's ledger (`get_transactions`, on the ledger or its archive) and books the deposit
/// paid only on exactly the transfer its payout asked (from the canister to the recipient, of
/// what it sent, its memo and `created_at_time`); a ledger not read answers `Unavailable`, to
/// be asked again. A refusal names what to search the ledger for.
#[update]
pub async fn resolve_deposit(
    r#ref: Vec<u8>,
    resolution: DepositResolution,
) -> Result<(), PoolError> {
    let caller = ic_cdk::api::caller();
    let r#ref = <[u8; 32]>::try_from(r#ref.as_slice())
        .map(types::hash::PoolRef::new)
        .map_err(|_| PoolError::NotAllowed("a deposit's ref is its 32 bytes".to_string()))?;
    let refund = pool::resolve::deposit::resolve(
        is_halted,
        ic_cdk::api::is_controller(&caller),
        ic_cdk::api::id(),
        r#ref,
        resolution,
        pool::now,
        is_sanctioned,
        pool::require_pool_open,
        &pool::payout::Live,
    )
    .await?;
    let Some(refund) = refund else {
        return Ok(());
    };
    pool::orphans::send(refund).await.map_err(|error| {
        PoolError::Unavailable(format!(
            "its refund was not signed, its USDC held as an orphan for resolve_orphan: {error:?}"
        ))
    })
}

/// Controller-only, refused while halted (Jeff, 2026-10-24): an ICP stake whose pull's answer
/// was lost, past the ledger's deduplication window (23 hours after it started; the canister
/// stops asking its pull again then), resolved once by its op id (its 32 bytes, as its
/// `StakeStarted` line names it). `Pulled { block }`: the controller saw the pull in that
/// ckUSDC ledger block; the canister reads the block (`get_transactions`, on the ledger or
/// its archive) and mints the stake's shares at the pool's price now only when it is the
/// stake's own pull (from its account to the canister's, by the canister, its amount, its op
/// id as the memo, its start as `created_at_time`), one block for one stake; a ledger not
/// read answers `Unavailable`, to be asked again. `NotPulled`: it never landed, and it is
/// dropped, its room freed.
#[update]
pub async fn resolve_stake(id: Vec<u8>, resolution: StakeResolution) -> Result<(), PoolError> {
    let caller = ic_cdk::api::caller();
    let id = <[u8; 32]>::try_from(id.as_slice())
        .map(types::hash::PoolOpId::new)
        .map_err(|_| PoolError::NotAllowed("a stake's id is its 32 bytes".to_string()))?;
    pool::resolve::stake::resolve(
        is_halted,
        ic_cdk::api::is_controller(&caller),
        ic_cdk::api::id(),
        id,
        resolution,
        pool::now,
        &pool::payout::Live,
    )
    .await
}

/// Any caller (the 65-byte EIP-712 signature proves the staker; the watcher's
/// `POST /v2/withdraw` forwards here), refused while halted, while the pool is closed, and for
/// a sanctioned party. Burns the shares `usdc` is worth (rounded up, at most `max_shares` and
/// the staker's own), keeps the rebalancing share in the pool and the network fee on Base, and
/// opens a payout of the rest from the Base pocket, waiting for its order or queued while the
/// pocket is short. One nonce per staker, once. Answers the payout's id and the shares burned.
#[update]
pub fn withdraw_evm(
    withdrawal: EvmWithdrawal,
    signature: Vec<u8>,
) -> Result<WithdrawEvmOk, PoolError> {
    pool::withdraw::withdraw_evm(withdrawal, signature, is_sanctioned).map(|(id, burned)| {
        WithdrawEvmOk {
            id,
            burned: candid::Nat::from(burned.get()),
        }
    })
}

/// Watcher-only, refused while halted and while the pool is closed: the order the quoter
/// built for waiting payout `id` at its exact amount (`abi.encode` of an Order), and the gas
/// limit to sign it with. The canister checks the untrusted order against the payout's own
/// terms (payer the canister, Base USDC in, exactly the payout's amount, no fee step, no CCTP
/// burn, one delivery paying the recipient at least their minimum of their token on their
/// chain, an Eco intent's reward refunding the canister, a deadline at least 60 s away and
/// within the payout's, no sanctioned party, the gas within `max_payout_gas`) and signs
/// `Swapic.run(order)` from its own Base pocket. A payout signed already answers its
/// transaction's id; a retry after a revert signs only the order it pinned.
#[update]
pub async fn pay_payout(id: u64, order: Vec<u8>, gas_limit: u64) -> Result<PayPayoutOk, PoolError> {
    pool::pay::pay_payout(id, order, gas_limit)
        .await
        .map(|tx_id| PayPayoutOk { tx_id })
}

/// Controller-only, refused while halted (Jeff, 2026-10-19: orphans are ALWAYS held for a
/// controller): resolves the orphan under `key` (its 48 bytes, as `pool_orphans` lists it)
/// once. `Refund { to }` signs `USDC.transfer(to, amount - min_network_fee)` on Base, the
/// fee kept for its gas; `Book` books it to the Base books' unattributed line.
#[update]
pub async fn resolve_orphan(key: Vec<u8>, action: OrphanAction) -> Result<(), PoolError> {
    let caller = ic_cdk::api::caller();
    pool::orphans::resolve(
        ic_cdk::api::is_controller(&caller),
        key,
        action,
        pool::now(),
        is_sanctioned,
    )
    .await
}
