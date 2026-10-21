//! The pool's rebalancing (spec 2026-10-19 section 3.4, the fixed interface section 3d): the
//! canister moves USDC between its Base pocket and its ICP pocket itself, one move per
//! direction at a time, and keeps the pool near its target split.
//!
//! A move is opened by the canister alone, from its books. It always has the canister's own
//! CCTP route; a route the watcher proposes may replace it within a window, after every check.
//!
//! - Base to ICP: the canister's order on Base burns USDC over CCTP to Ethereum with a
//!   `CkDeposit` envelope for its OWN principal and the rebalance subaccount; the ck minter
//!   mints ckUSDC there, and the canister sweeps it into its main account. By Eco: the fill
//!   delivers to the canister's Ethereum address, and the canister makes the ck deposit.
//! - ICP to Base: the ck minter's `withdraw_erc20` of ckUSDC to the canister's Ethereum
//!   address, then the canister's order on Ethereum burns it over CCTP to its Base address
//!   (or an Eco intent delivers it there).
//!
//! The lines a move keeps: opening reserves the amount in its source pocket; leaving it (the
//! first signature on Base, the minter's call on ICP) moves it to `in_flight`, where the
//! balance checks expect it nowhere; the arrival credits the destination with what arrived,
//! the difference being the move's cost (`RebalanceLines::cost`).
//!
//! Pure: no canister calls. Stored as minicbor: `#[n]` indices are append-only, never
//! renumbered or reused, and a new field is optional.

#[cfg(test)]
mod tests;

use super::payout::PinnedOrder;
use crate::hash::{PoolRef, TxHash};
use crate::numeric::{BlockNumber, Timestamp, TokenAmount};
use alloy_primitives::keccak256;
use minicbor::{Decode, Encode};
use std::collections::BTreeSet;
use std::time::Duration;
use thiserror::Error;

/// `keccak256("swapic.pool.v1.rebalance")`: the canister's one rebalance subaccount, where
/// the ck minter mints a Base to ICP move's ckUSDC. Never a user's.
pub fn rebalance_subaccount() -> [u8; 32] {
    keccak256(b"swapic.pool.v1.rebalance").0
}

/// A move's ref, `keccak256("swapic.pool.v1.move" ‖ id)` (the id as 8 bytes, big endian):
/// the `Order.ref` and the envelope's ref of every route for it.
pub fn move_ref(id: u64) -> PoolRef {
    let mut preimage = b"swapic.pool.v1.move".to_vec();
    preimage.extend_from_slice(&id.to_be_bytes());
    PoolRef::new(keccak256(&preimage).0)
}

/// The ck helper's word for the principal a CkDeposit credits: its length in the first byte,
/// its bytes after it, zeros after them (`swapic::ck_principal` reads it back).
pub fn ck_word(principal: &candid::Principal) -> [u8; 32] {
    let bytes = principal.as_slice();
    let mut word = [0; 32];
    word[0] = u8::try_from(bytes.len()).expect("BUG: a principal is 29 bytes at most");
    word[1..1 + bytes.len()].copy_from_slice(bytes);
    word
}

/// A transaction the canister signed whose CCTP burn it finishes itself, with no fee step:
/// the source of a move's or a payout's own transfer, indexed by its hash once it succeeded
/// at depth.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub enum OwnSource {
    #[n(0)]
    Move(#[n(0)] u64),
    #[n(1)]
    Payout(#[n(0)] u64),
}

crate::storable_as_cbor!(OwnSource);

alloy_sol_types::sol! {
    /// The ck minter's ERC-20 deposit helper on Ethereum.
    function depositErc20(address erc20Address, uint256 amount, bytes32 principal, bytes32 subaccount) external;
}

/// A CCTP v2 `depositForBurnWithHook` call's data, read back: None for data that is not one.
pub fn read_burn(data: &[u8]) -> Option<crate::swapic::depositForBurnWithHookCall> {
    use alloy_sol_types::SolCall;
    crate::swapic::depositForBurnWithHookCall::abi_decode(data).ok()
}

/// A CCTP v2 `depositForBurnWithHook` call's data.
pub fn burn_data(call: &crate::swapic::depositForBurnWithHookCall) -> Vec<u8> {
    use alloy_sol_types::SolCall;
    call.abi_encode()
}

/// The ck helper's `depositErc20(token, amount, principal, subaccount)`: a deposit for the
/// principal the word names, at that subaccount.
pub fn ck_deposit_data(
    token: crate::evm::EvmAddress,
    amount: TokenAmount,
    principal: [u8; 32],
    subaccount: [u8; 32],
) -> Vec<u8> {
    use alloy_sol_types::SolCall;
    depositErc20Call {
        erc20Address: crate::swapic::addr(token),
        amount: alloy_primitives::U256::from_be_bytes(amount.to_be_bytes()),
        principal: alloy_primitives::FixedBytes(principal),
        subaccount: alloy_primitives::FixedBytes(subaccount),
    }
    .abi_encode()
}

/// How long the daily cap looks back.
pub const DAY: Duration = Duration::from_secs(24 * 3_600);

/// The longest proposal window the settings take.
pub const MAX_PROPOSAL_WINDOW: Duration = Duration::from_secs(600);

/// The highest proposal fee cap the settings take: 1%.
pub const MAX_PROPOSAL_FEE_BPS: u16 = 100;

/// Which way a move carries USDC.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Encode, Decode)]
#[cbor(index_only)]
pub enum MoveDirection {
    #[n(0)]
    BaseToIcp,
    #[n(1)]
    IcpToBase,
}

/// The rebalancing knobs (interface section 3d's `RebalanceSettings`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct RebalanceSettings {
    /// The ICP pocket's target share of the pool, in bps.
    #[n(0)]
    pub target_icp_bps: u16,
    /// How far the share may drift either way before a move opens.
    #[n(1)]
    pub band_bps: u16,
    /// The least move, micro USDC, unless a payout waits.
    #[n(2)]
    pub min_move: TokenAmount,
    /// The most one move carries.
    #[n(3)]
    pub max_move: TokenAmount,
    /// The most the moves opened within a day carry.
    #[n(4)]
    pub daily_cap: TokenAmount,
    /// How long proposals are taken for a move that is not urgent.
    #[n(5)]
    pub proposal_window: Duration,
    /// The most a proposal's fee may be, in bps of the move's amount.
    #[n(6)]
    pub max_proposal_fee_bps: u16,
    /// The own route burns at standard finality (no Circle fee) unless a payout waits; off,
    /// it always burns fast.
    #[n(7)]
    pub standard_when_idle: bool,
}

impl Default for RebalanceSettings {
    /// 40% ICP, plus or minus 15%; $300 the least move (Jeff, 2026-10-21), $5,000 the most,
    /// $20,000 a day; a 60 s window; a 2 bps fee cap; standard finality when idle.
    fn default() -> Self {
        Self {
            target_icp_bps: 4_000,
            band_bps: 1_500,
            min_move: TokenAmount::new(300_000_000),
            max_move: TokenAmount::new(5_000_000_000),
            daily_cap: TokenAmount::new(20_000_000_000),
            proposal_window: Duration::from_secs(60),
            max_proposal_fee_bps: 2,
            standard_when_idle: true,
        }
    }
}

/// Why rebalance settings are not ones the pool runs on.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RebalanceSettingsError {
    #[error("the target and its band must stay within 0 and 10000 bps")]
    Band,
    #[error("{0} must be above zero")]
    Zero(&'static str),
    #[error("max_move is below min_move")]
    MaxBelowMin,
    #[error("daily_cap is below max_move")]
    DailyBelowMax,
    #[error("proposal_window_s is above {}", MAX_PROPOSAL_WINDOW.as_secs())]
    Window,
    #[error("max_proposal_fee_bps is above {MAX_PROPOSAL_FEE_BPS}")]
    FeeCap,
}

impl RebalanceSettings {
    /// The knobs hold together: the target and its band inside the pool, a least move above
    /// zero and below the most, a daily cap at least one move, a window of at most ten
    /// minutes (zero takes no proposals), a fee cap of at most 1%.
    pub fn validate(&self) -> Result<(), RebalanceSettingsError> {
        if self.band_bps > self.target_icp_bps
            || u32::from(self.target_icp_bps) + u32::from(self.band_bps) > 10_000
        {
            return Err(RebalanceSettingsError::Band);
        }
        if self.min_move == TokenAmount::ZERO {
            return Err(RebalanceSettingsError::Zero("min_move"));
        }
        if self.max_move < self.min_move {
            return Err(RebalanceSettingsError::MaxBelowMin);
        }
        if self.daily_cap < self.max_move {
            return Err(RebalanceSettingsError::DailyBelowMax);
        }
        if self.proposal_window > MAX_PROPOSAL_WINDOW {
            return Err(RebalanceSettingsError::Window);
        }
        if self.max_proposal_fee_bps > MAX_PROPOSAL_FEE_BPS {
            return Err(RebalanceSettingsError::FeeCap);
        }
        Ok(())
    }

    /// The most a proposal for a move of `amount` may pay in fees, rounded down.
    pub fn proposal_fee_cap(&self, amount: TokenAmount) -> TokenAmount {
        amount
            .checked_mul(self.max_proposal_fee_bps)
            .and_then(|scaled| scaled.checked_div_floor(10_000_u32))
            .unwrap_or(TokenAmount::ZERO)
    }
}

/// How a move goes: the canister's own CCTP route, or a proposal it took.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub enum MoveRoute {
    /// The canister's own order: a CCTP burn offering Circle at most `max_fee`, fast or at
    /// standard finality.
    #[n(0)]
    Own {
        #[n(0)]
        max_fee: TokenAmount,
        #[n(1)]
        fast: bool,
    },
    /// The watcher's proposal, an Eco intent keeping `fee` of the move.
    #[n(1)]
    Proposed {
        #[n(0)]
        fee: TokenAmount,
    },
}

impl MoveRoute {
    /// The most the route may keep of the move: what its arrival is credited at the most less.
    pub fn cost(&self) -> TokenAmount {
        match self {
            MoveRoute::Own { max_fee, .. } => *max_fee,
            MoveRoute::Proposed { fee } => *fee,
        }
    }

    /// Whether it goes by Eco (a proposal).
    pub fn is_eco(&self) -> bool {
        matches!(self, MoveRoute::Proposed { .. })
    }
}

/// Where a move stands: its last proven position. Whether it is stuck is its own flag
/// (`Move::stuck`), so a stuck move keeps the position a retry starts from.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub enum MoveStatus {
    /// Its amount is reserved in its source pocket. Base to ICP: it waits for its window and
    /// its route. ICP to Base: it waits for the minter's call.
    #[n(0)]
    Open,
    /// ICP to Base: the ck minter's `withdraw_erc20` is out, the ckUSDC in flight.
    #[n(1)]
    Withdrawing,
    /// ICP to Base: the minter took the ckUSDC; its USDC is on its way to Ethereum.
    #[n(2)]
    Withdrawn,
    /// Its USDC is at the canister's Ethereum address: an ICP to Base move's from the minter
    /// (it waits for its window and its route), or a Base to ICP move's Eco fill (it waits for
    /// its ck deposit).
    #[n(3)]
    Landed,
    /// Its order is pinned and waits for its signature.
    #[n(4)]
    Routed,
    /// A transaction of it is out (its order, or a Base to ICP Eco move's ck deposit): the log
    /// index of the line that recorded it, its id in `signed_txs`.
    #[n(5)]
    Signed(#[n(0)] u64),
    /// That transaction succeeded at depth: its money is on the rail.
    #[n(6)]
    Burned,
    /// Base to ICP: the sweep of the rebalance subaccount into the canister's main account is
    /// out.
    #[n(7)]
    Sweeping,
    #[n(8)]
    Arrived,
    /// A controller wrote it off.
    #[n(9)]
    Lost(#[n(0)] String),
    /// Its money came back to its source pocket, why: an ICP to Base move the ck minter
    /// reimbursed (P11e).
    #[n(10)]
    Returned(#[n(0)] String),
}

impl MoveStatus {
    /// Whether nothing more happens to it.
    pub fn is_final(&self) -> bool {
        matches!(
            self,
            MoveStatus::Arrived | MoveStatus::Lost(_) | MoveStatus::Returned(_)
        )
    }
}

/// The cheapest valid proposal a move took so far: the order pinned, and what Eco keeps.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub struct Proposal {
    #[n(0)]
    pub order: PinnedOrder,
    #[n(1)]
    pub fee: TokenAmount,
}

/// USDC that reached the canister's Ethereum address for a move.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct Landing {
    #[n(0)]
    pub tx: TxHash,
    #[n(1)]
    pub block: BlockNumber,
    #[n(2)]
    pub amount: TokenAmount,
}

/// A Base to ICP move's sweep: the dedup pair of the transfer from the rebalance subaccount,
/// what it moves, and what the move is credited.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct Sweep {
    #[n(0)]
    pub created_at: Timestamp,
    #[n(1)]
    pub amount: TokenAmount,
    #[n(2)]
    pub credited: TokenAmount,
}

/// One move.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub struct Move {
    #[n(0)]
    pub id: u64,
    #[n(1)]
    pub direction: MoveDirection,
    /// Micro USDC, what leaves the source pocket.
    #[n(2)]
    pub amount: TokenAmount,
    /// Opened for a waiting payout: no window, the fast route.
    #[n(3)]
    pub urgent: bool,
    #[n(4)]
    pub opened_at: Timestamp,
    /// The own route's estimated cost when it opened, micro USDC.
    #[n(5)]
    pub own_cost: TokenAmount,
    #[n(6)]
    pub status: MoveStatus,
    /// Proposals are taken until then: from the opening (Base to ICP) or the landing on
    /// Ethereum (ICP to Base). None for an urgent move.
    #[n(7)]
    pub window_ends: Option<Timestamp>,
    #[n(8)]
    pub proposal: Option<Proposal>,
    #[n(9)]
    pub route: Option<MoveRoute>,
    /// The order the route pinned: a retry signs exactly this one.
    #[n(10)]
    pub order: Option<PinnedOrder>,
    /// The last transaction signed for it.
    #[n(11)]
    pub tx_hash: Option<TxHash>,
    /// Whether its amount left the source pocket (`in_flight` there).
    #[n(12)]
    pub left: bool,
    /// ICP to Base: the minter call's `created_at_time`, the dedup instant of its approvals.
    #[n(13)]
    pub minter_at: Option<Timestamp>,
    /// ICP to Base: the minter's withdrawal id.
    #[n(14)]
    pub withdrawal: Option<u64>,
    #[n(15)]
    pub landed: Option<Landing>,
    /// The Eco intent its order published, read off its receipt.
    #[cbor(n(16), with = "minicbor::bytes")]
    pub intent: Option<[u8; 32]>,
    #[n(17)]
    pub sweep: Option<Sweep>,
    /// What its destination was credited.
    #[n(18)]
    pub arrived: Option<TokenAmount>,
    /// Why it stopped for a controller.
    #[n(19)]
    pub stuck: Option<String>,
    /// When its position last changed: what its wait is measured from.
    #[n(20)]
    pub since: Timestamp,
    /// Base to ICP by Eco: the Ethereum block its fill is looked for from.
    #[n(21)]
    pub scan_from: Option<BlockNumber>,
    /// How many times its order reverted: none before the first.
    #[n(22)]
    pub reverts: Option<u32>,
}

crate::storable_as_cbor!(Move);

impl Move {
    /// A move as `MoveOpened` opens it.
    pub fn opened(
        id: u64,
        direction: MoveDirection,
        amount: TokenAmount,
        urgent: bool,
        opened_at: Timestamp,
        window_ends: Option<Timestamp>,
        own_cost: TokenAmount,
    ) -> Self {
        Self {
            id,
            direction,
            amount,
            urgent,
            opened_at,
            own_cost,
            status: MoveStatus::Open,
            window_ends,
            proposal: None,
            route: None,
            order: None,
            tx_hash: None,
            left: false,
            minter_at: None,
            withdrawal: None,
            landed: None,
            intent: None,
            sweep: None,
            arrived: None,
            stuck: None,
            since: opened_at,
            scan_from: None,
            reverts: None,
        }
    }

    pub fn r#ref(&self) -> PoolRef {
        move_ref(self.id)
    }

    /// Whether a proposal may be taken for it now, its window aside: not urgent, unrouted, not
    /// stuck, and where a proposal can help (Base to ICP from its opening; ICP to Base once
    /// its USDC landed on Ethereum, as only the Ethereum to Base leg can go by Eco).
    pub fn proposable(&self) -> bool {
        let at = match self.direction {
            MoveDirection::BaseToIcp => self.status == MoveStatus::Open,
            MoveDirection::IcpToBase => self.status == MoveStatus::Landed,
        };
        at && !self.urgent && self.route.is_none() && self.stuck.is_none()
    }

    /// Whether its route may be decided now, the window aside: where a proposal could be
    /// taken, urgent or not.
    pub fn routable(&self) -> bool {
        let at = match self.direction {
            MoveDirection::BaseToIcp => self.status == MoveStatus::Open,
            MoveDirection::IcpToBase => self.status == MoveStatus::Landed,
        };
        at && self.route.is_none() && self.stuck.is_none()
    }

    /// What its arrival may be credited at the most: its amount, less what its route may
    /// keep. A donation to where it arrives is never the pool's.
    pub fn creditable(&self) -> TokenAmount {
        let cost = self.route.map_or(TokenAmount::ZERO, |route| route.cost());
        self.amount.checked_sub(cost).unwrap_or(TokenAmount::ZERO)
    }
}

/// The rebalancing's lines in the pool ledger, absent until its first step.
#[derive(Clone, Debug, Default, PartialEq, Eq, Encode, Decode)]
pub struct RebalanceLines {
    #[n(0)]
    pub settings: Option<RebalanceSettings>,
    /// The id the next move opens with.
    #[n(1)]
    pub next_id: u64,
    /// The moves not arrived or written off.
    #[n(2)]
    pub open: BTreeSet<u64>,
    /// When each move of the last day opened and what it carries: the daily cap's reading.
    #[n(3)]
    pub recent: Vec<(Timestamp, TokenAmount)>,
    /// What the moves cost the pool: the rails' fees, the ledger fees.
    #[n(4)]
    pub cost: TokenAmount,
    /// What controllers wrote off.
    #[n(5)]
    pub lost: TokenAmount,
}

impl RebalanceLines {
    /// What the moves opened within a day before `at` carry.
    pub fn opened_within_day(&self, at: Timestamp) -> TokenAmount {
        let since = at.checked_sub(DAY).unwrap_or(Timestamp::from_nanos(0));
        self.recent
            .iter()
            .filter(|(opened, _)| *opened > since)
            .try_fold(TokenAmount::ZERO, |sum, (_, amount)| {
                sum.checked_add(*amount)
            })
            .unwrap_or(TokenAmount::MAX)
    }
}
