//! The pool's update doors (interface section 3).

use crate::guards::require_quoter;
use crate::pool;
use crate::storage::halt::is_halted;
use crate::storage::sanctions::is_sanctioned;
use ic_cdk::update;
pub use settlement_api::types::pool::{
    PoolArrival, PoolArrivalOutcome, PoolDeposit, PoolError, PoolSettings, StakeIcpArgs,
    StakeIcpOk, WithdrawIcpArgs, WithdrawIcpOk,
};
use types::pool::PoolStep;

/// Controller-only, refused while halted. Sets the pool's knobs after reading the ckUSDC
/// ledger's fee and every listed token's (`icrc1_fee`), which the registrations' premium
/// floor reads.
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

/// Quoter-only, refused while halted and while the pool is closed. Registers a deposit at
/// the user's commit (interface section 4): the ref must be the one its terms commit to,
/// the fee included; it expires within 15 minutes; its token is listed; its payer and owner
/// are not sanctioned; its payer and its owner hold fewer than `max_open_per_payer` open
/// registrations; a swap is within the per-swap cap, carries at least the premium's floor
/// and, on a ckUSDC payout, pays at least its own minimum, and reserves `min_usdc - premium`
/// of the ICP pocket within `max_held_bps` of it; a stake holds its TVL room. The hold is
/// released when it expires.
#[update]
pub fn register_pool_deposit(deposit: PoolDeposit) -> Result<(), PoolError> {
    require_quoter().map_err(pool::guard_error)?;
    pool::require_running()?;
    pool::require_pool_open()?;
    let (r#ref, terms) = deposit.into_domain()?;
    pool::register::register(r#ref, terms, pool::now(), is_sanctioned)
}

/// Watcher-only, refused while halted. The watcher saw a USDC transfer to the canister's
/// Base address in `base_tx` (and, for an Eco arrival, the order's `src_tx` on `src_chain`;
/// an empty `src_tx` names none). The canister reads the receipt itself (one batched
/// outcall, and one more on the source chain for Eco) at the crediting depth, proves every
/// transfer to it against its rail, and credits each at most once: `Credited` (the payout
/// follows on the canister's timer), or `Unattributed` with why, kept for the refund.
#[update]
pub async fn note_pool_arrival(arrival: PoolArrival) -> Result<PoolArrivalOutcome, PoolError> {
    pool::arrival::note(arrival).await
}

/// Any caller, refused while halted, while the pool is closed, and for a sanctioned party.
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

/// Quoter-only, refused while halted. After a commit that met `Known`, whether the
/// registration stored under `deposit.ref` is this one: the same kind, fees, `min_usdc`,
/// `src_payer`, rail, expiry and salt. `Unknown` when none is stored. Reads only, and writes
/// no line.
#[update]
pub fn pool_deposit_agreed(deposit: PoolDeposit) -> Result<bool, PoolError> {
    require_quoter().map_err(pool::guard_error)?;
    pool::require_running()?;
    let (r#ref, terms) = deposit.into_domain()?;
    crate::storage::events::read_state(|state| pool::register::agreed(state, r#ref, &terms))
}
