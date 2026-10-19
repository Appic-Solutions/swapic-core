//! The pool's update doors (interface section 3).

use crate::guards::require_quoter;
use crate::pool;
use crate::storage::halt::is_halted;
use crate::storage::sanctions::is_sanctioned;
use ic_cdk::update;
pub use settlement_api::types::pool::{
    PoolArrival, PoolArrivalOutcome, PoolDeposit, PoolError, PoolSettings,
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
