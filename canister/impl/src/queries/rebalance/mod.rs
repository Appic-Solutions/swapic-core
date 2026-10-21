//! What the rebalancing's fold holds (interface section 3d).

use crate::pool::rebalance;
use crate::storage::events::read_state;
use ic_cdk::query;
pub use settlement_api::types::pool::{MoveView, RebalanceSettings};

/// The open and in-flight moves, in id order.
#[query]
pub fn rebalance_moves() -> Vec<MoveView> {
    rebalance::views()
}

/// The rebalancing's knobs; the defaults while none are set.
#[query]
pub fn rebalance_settings() -> RebalanceSettings {
    RebalanceSettings::from(&read_state(rebalance::settings_in))
}
