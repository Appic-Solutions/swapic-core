//! The rebalancing's candid types: interface section 3d, field for field, and their
//! conversions to and from the domain (`types::pool::rebalance`).

#[cfg(test)]
mod tests;

use super::{amount, nat, PoolWireError};
use candid::{CandidType, Nat};
use serde::Deserialize;
use std::time::Duration;
use types::pool::rebalance as domain;

#[derive(CandidType, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveDirection {
    BaseToIcp,
    IcpToBase,
}

/// Where a move stands. `Signed` is the id of its transaction in `signed_txs`.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum MoveStatus {
    Open,
    Routed,
    Signed(u64),
    Burned,
    Landed,
    Arrived,
    Stuck(String),
}

/// One move.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct MoveView {
    pub id: u64,
    pub direction: MoveDirection,
    /// Micro USDC.
    pub amount: Nat,
    pub urgent: bool,
    pub opened_at_ns: u64,
    /// Proposals are accepted until then; none for an urgent move.
    pub window_ends_ns: Option<u64>,
    /// Micro USDC, the canister's own CCTP route estimate.
    pub own_route_cost: Nat,
    pub status: MoveStatus,
    /// The move's ref (32 bytes): the `Order.ref` and envelope ref of any route for it.
    pub r#ref: Vec<u8>,
    pub arrived: Option<Nat>,
}

#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct RebalanceSettings {
    pub target_icp_bps: u16,
    pub band_bps: u16,
    pub min_move: Nat,
    pub max_move: Nat,
    pub daily_cap: Nat,
    pub proposal_window_s: u64,
    pub max_proposal_fee_bps: u16,
    pub standard_when_idle: bool,
}

/// The controller's answer to a stuck move (`resolve_move`): send it on from where it
/// stands, or write it off with why.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum MoveResolution {
    Retry,
    Lost(String),
}

impl From<domain::MoveDirection> for MoveDirection {
    fn from(direction: domain::MoveDirection) -> Self {
        match direction {
            domain::MoveDirection::BaseToIcp => Self::BaseToIcp,
            domain::MoveDirection::IcpToBase => Self::IcpToBase,
        }
    }
}

impl From<MoveDirection> for domain::MoveDirection {
    fn from(direction: MoveDirection) -> Self {
        match direction {
            MoveDirection::BaseToIcp => Self::BaseToIcp,
            MoveDirection::IcpToBase => Self::IcpToBase,
        }
    }
}

/// The interface's status of a move: its own stuck flag first; then its position, the
/// legs the interface does not name read as the one they belong to (the minter's call is
/// still `Open`, the minter's burn and the sweep are `Burned`), a write-off as `Stuck`.
impl From<&domain::Move> for MoveStatus {
    fn from(item: &domain::Move) -> Self {
        if let (Some(why), false) = (&item.stuck, item.status.is_final()) {
            return Self::Stuck(why.clone());
        }
        match &item.status {
            domain::MoveStatus::Open | domain::MoveStatus::Withdrawing => Self::Open,
            domain::MoveStatus::Withdrawn
            | domain::MoveStatus::Burned
            | domain::MoveStatus::Sweeping => Self::Burned,
            domain::MoveStatus::Landed => Self::Landed,
            domain::MoveStatus::Routed => Self::Routed,
            domain::MoveStatus::Signed(id) => Self::Signed(*id),
            domain::MoveStatus::Arrived => Self::Arrived,
            domain::MoveStatus::Lost(why) => Self::Stuck(format!("written off: {why}")),
        }
    }
}

impl From<&domain::Move> for MoveView {
    fn from(item: &domain::Move) -> Self {
        Self {
            id: item.id,
            direction: item.direction.into(),
            amount: nat(item.amount),
            urgent: item.urgent,
            opened_at_ns: item.opened_at.as_nanos(),
            window_ends_ns: item.window_ends.map(|at| at.as_nanos()),
            own_route_cost: nat(item.own_cost),
            status: MoveStatus::from(item),
            r#ref: item.r#ref().into_bytes().to_vec(),
            arrived: item.arrived.map(nat),
        }
    }
}

impl TryFrom<RebalanceSettings> for domain::RebalanceSettings {
    type Error = PoolWireError;

    fn try_from(settings: RebalanceSettings) -> Result<Self, Self::Error> {
        Ok(Self {
            target_icp_bps: settings.target_icp_bps,
            band_bps: settings.band_bps,
            min_move: amount("min_move", &settings.min_move)?,
            max_move: amount("max_move", &settings.max_move)?,
            daily_cap: amount("daily_cap", &settings.daily_cap)?,
            proposal_window: Duration::from_secs(settings.proposal_window_s),
            max_proposal_fee_bps: settings.max_proposal_fee_bps,
            standard_when_idle: settings.standard_when_idle,
        })
    }
}

impl From<&domain::RebalanceSettings> for RebalanceSettings {
    fn from(settings: &domain::RebalanceSettings) -> Self {
        Self {
            target_icp_bps: settings.target_icp_bps,
            band_bps: settings.band_bps,
            min_move: nat(settings.min_move),
            max_move: nat(settings.max_move),
            daily_cap: nat(settings.daily_cap),
            proposal_window_s: settings.proposal_window.as_secs(),
            max_proposal_fee_bps: settings.max_proposal_fee_bps,
            standard_when_idle: settings.standard_when_idle,
        }
    }
}
