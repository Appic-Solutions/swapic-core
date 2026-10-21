//! The rebalancing's update doors (interface section 3d).

use crate::pool::rebalance;
use crate::storage::halt::is_halted;
use ic_cdk::update;
pub use settlement_api::types::pool::{PoolError, RebalanceSettings};

/// Controller-only, refused while halted: the rebalancing's knobs, held together.
#[update]
pub fn set_rebalance_settings(settings: RebalanceSettings) -> Result<(), PoolError> {
    let caller = ic_cdk::api::caller();
    rebalance::set_settings(is_halted(), ic_cdk::api::is_controller(&caller), settings)
}
