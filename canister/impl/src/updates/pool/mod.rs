//! The pool's update doors (interface section 3).

use crate::pool;
use crate::storage::halt::is_halted;
use ic_cdk::update;
pub use settlement_api::types::pool::{PoolError, PoolSettings};
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
        .map(drop)
        .map_err(|error| PoolError::BadSettings(error.to_string()))
}
