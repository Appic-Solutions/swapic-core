//! The rebalancing on the canister's side (spec 3.4, interface section 3d): its settings,
//! its moves as the fold holds them, and their views. The rules are `types::pool`'s; the
//! modules below read the clock, the config and the chains, and append.

#[cfg(test)]
mod tests;

pub mod cost;
pub mod trigger;

use crate::state::{State, Store};
use crate::storage::events::read_state;
use settlement_api::types::pool::{
    MoveView, PoolError, RebalanceSettings as WireSettings,
};
use types::pool::{Move, PoolStep, RebalanceSettings};

/// The knobs as the domain takes them, or the refusal: the halt, then the controller, then
/// settings that hold together.
pub fn checked(
    halted: bool,
    is_controller: bool,
    settings: WireSettings,
) -> Result<RebalanceSettings, PoolError> {
    if halted {
        return Err(PoolError::Halted);
    }
    if !is_controller {
        return Err(PoolError::NotAllowed(
            "only a controller sets the rebalancing's settings".to_string(),
        ));
    }
    let settings = RebalanceSettings::try_from(settings)
        .map_err(|error| PoolError::BadSettings(error.to_string()))?;
    settings
        .validate()
        .map_err(|error| PoolError::BadSettings(error.to_string()))?;
    Ok(settings)
}

/// `set_rebalance_settings`' body.
pub fn set_settings(
    halted: bool,
    is_controller: bool,
    settings: WireSettings,
) -> Result<(), PoolError> {
    let settings = checked(halted, is_controller, settings)?;
    crate::storage::pool::record(PoolStep::MoveSettingsSet { settings })
        .map_err(|error| PoolError::BadSettings(error.to_string()))?;
    Ok(())
}

/// The knobs set, or the defaults before any are.
pub fn settings_in<S: Store>(state: &State<S>) -> RebalanceSettings {
    state.pool().moves().settings.unwrap_or_default()
}

/// The moves not arrived or written off, in id order.
pub fn open_moves<S: Store>(state: &State<S>) -> Vec<Move> {
    state
        .pool()
        .moves()
        .open
        .into_iter()
        .filter_map(|id| state.store().pool_move(id))
        .collect()
}

/// One move, by id.
pub fn item(id: u64) -> Option<Move> {
    read_state(|state| state.store().pool_move(id))
}

/// Drives every open move a step on at `now` (P11c Task 4 fills this in).
pub async fn drive(_now: types::Timestamp) {}

/// `rebalance_moves`: the open and in-flight moves.
pub fn views() -> Vec<MoveView> {
    read_state(open_moves).iter().map(MoveView::from).collect()
}
