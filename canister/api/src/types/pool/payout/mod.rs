//! The Base payouts' candid types: interface section 3b, field for field, and their
//! conversions to and from the domain (`types::pool::payout`).

#[cfg(test)]
mod tests;

use super::{amount, evm_address, nat, IcpAccount, PoolWireError};
use candid::{CandidType, Nat, Principal};
use serde::Deserialize;
use std::time::Duration;
use types::pool::payout as domain;
use types::{ChainId, Timestamp};

/// Where and how a payout pays the user.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PayoutTerms {
    /// A chain the canister signs on, Base included.
    pub dst_chain: u64,
    /// An EVM address.
    pub recipient: String,
    /// An EVM token address, or `0xEeee...EEeE` for the native coin.
    pub token_out: String,
    /// In `token_out`'s units.
    pub min_out: Nat,
    /// After this, nothing is signed and the payout is undone.
    pub deadline_ns: u64,
}

/// An ICP user's swap to an EVM token through the pool.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct IcpSwapTerms {
    /// ckUSDC or a listed ICP token.
    pub token_in: Principal,
    /// Pulled with `icrc2_transfer_from` from the caller's account.
    pub amount_in: Nat,
    pub from_subaccount: Option<Vec<u8>>,
    /// The least ckUSDC the ICPSwap leg must give (`amount_in` for ckUSDC).
    pub min_usdc: Nat,
    /// Micro USDC, at least the pool's premium floor.
    pub premium: Nat,
    /// Micro USDC, at least `payout_settings.min_network_fee`.
    pub network_fee: Nat,
    pub payout: PayoutTerms,
    /// Default: the caller's account.
    pub refund_to: Option<IcpAccount>,
}

/// An EVM staker's withdrawal, signed by the staker (EIP-712).
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct EvmWithdrawal {
    /// The EVM address owning the shares, the EIP-712 signer.
    pub staker: String,
    /// Micro USDC taken out of the pool (shares burned = `ceil(usdc * S / A)`).
    pub usdc: Nat,
    pub max_shares: Nat,
    /// At least the canister's floor.
    pub rebalance_share: Nat,
    pub network_fee: Nat,
    pub payout: PayoutTerms,
    /// Once per staker.
    pub nonce: u64,
}

#[derive(CandidType, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum PayoutKind {
    IcpSwap,
    Withdrawal,
}

#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum PayoutStatus {
    Pulling,
    Swapping,
    Queued,
    Waiting,
    Signed(u64),
    Paid,
    /// Its order ran on Base and published an Eco intent: paid once the intent's fill is read.
    Delivering,
    /// Its refund is on its way, with why: a revert, an Eco intent never filled, or its
    /// deadline (nothing is signed for it again).
    Refunding(String),
    Undone(String),
    Failed(String),
}

#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PayoutView {
    pub id: u64,
    pub kind: PayoutKind,
    pub terms: PayoutTerms,
    /// The exact USDC the payout order must spend on Base.
    pub amount: Option<Nat>,
    pub status: PayoutStatus,
    pub order_id: Option<Vec<u8>>,
    pub tx_hash: Option<Vec<u8>>,
}

#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PayoutWaiting {
    pub id: u64,
    pub kind: PayoutKind,
    pub amount: Nat,
    pub terms: PayoutTerms,
    /// Micro USDC, the payout's own network fee (P11e): the watcher chooses CCTP only when it
    /// covers the canister's CCTP costs.
    pub network_fee: Option<Nat>,
}

#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PayoutSettings {
    /// Micro USDC, the least a payout's network fee may be.
    pub min_network_fee: Nat,
    /// The least a stake's or a withdrawal's rebalancing share may be, in bps of its amount.
    pub rebalance_share_floor_bps: u16,
    /// How long a waiting payout waits for a valid order before it is retried, then undone
    /// at its deadline.
    pub order_wait_s: u64,
    /// The most gas a payout order may be signed with on Base.
    pub max_payout_gas: u64,
    /// Micro USDC, the least network fee a payout over CCTP carries (the user's line for the
    /// canister's own finish on the destination). None: no CCTP payout is signed.
    pub cctp_min_network_fee: Option<Nat>,
}

impl TryFrom<PayoutTerms> for domain::PayoutTerms {
    type Error = PoolWireError;

    fn try_from(terms: PayoutTerms) -> Result<Self, Self::Error> {
        Ok(Self {
            dst_chain: ChainId::new(terms.dst_chain),
            recipient: evm_address("recipient", &terms.recipient)?,
            token_out: evm_address("token_out", &terms.token_out)?,
            min_out: amount("min_out", &terms.min_out)?,
            deadline: Timestamp::from_nanos(terms.deadline_ns),
        })
    }
}

impl From<domain::PayoutTerms> for PayoutTerms {
    fn from(terms: domain::PayoutTerms) -> Self {
        Self {
            dst_chain: terms.dst_chain.get(),
            recipient: terms.recipient.to_string(),
            token_out: terms.token_out.to_string(),
            min_out: nat(terms.min_out),
            deadline_ns: terms.deadline.as_nanos(),
        }
    }
}

impl From<&domain::PayoutKind> for PayoutKind {
    fn from(kind: &domain::PayoutKind) -> Self {
        match kind {
            domain::PayoutKind::IcpSwap(_) => Self::IcpSwap,
            domain::PayoutKind::Withdrawal(_) => Self::Withdrawal,
        }
    }
}

impl From<&domain::PayoutStatus> for PayoutStatus {
    /// A payout opened is pulling (an ICP swap's pull is out); one whose amount was just
    /// learnt is still swapping until the same message queues it or lets it wait; one whose
    /// transaction reverted, or whose undo began, is refunding (nothing is signed for it
    /// again).
    fn from(status: &domain::PayoutStatus) -> Self {
        match status {
            domain::PayoutStatus::Opened => Self::Pulling,
            domain::PayoutStatus::Swapping | domain::PayoutStatus::Known => Self::Swapping,
            domain::PayoutStatus::Queued => Self::Queued,
            domain::PayoutStatus::Waiting => Self::Waiting,
            domain::PayoutStatus::Signed(tx_id) => Self::Signed(*tx_id),
            domain::PayoutStatus::Paid => Self::Paid,
            domain::PayoutStatus::Reverted => {
                Self::Refunding("its transaction reverted".to_string())
            }
            domain::PayoutStatus::Delivering => Self::Delivering,
            domain::PayoutStatus::Unfilled => {
                Self::Refunding("its Eco intent was not filled".to_string())
            }
            domain::PayoutStatus::Undoing(why) => Self::Refunding(why.clone()),
            domain::PayoutStatus::Undone(why) => Self::Undone(why.clone()),
            domain::PayoutStatus::Failed(why) => Self::Failed(why.clone()),
        }
    }
}

impl From<&domain::EvmPayout> for PayoutView {
    fn from(payout: &domain::EvmPayout) -> Self {
        Self {
            id: payout.id,
            kind: (&payout.kind).into(),
            terms: payout.terms.into(),
            amount: payout.amount.map(nat),
            status: (&payout.status).into(),
            order_id: payout.order.as_ref().map(|order| order.order_id.to_vec()),
            tx_hash: payout.tx_hash.map(|hash| hash.into_bytes().to_vec()),
        }
    }
}

impl PayoutWaiting {
    /// A payout waiting for an order, None for one whose amount is not known.
    pub fn of(payout: &domain::EvmPayout) -> Option<Self> {
        Some(Self {
            id: payout.id,
            kind: (&payout.kind).into(),
            amount: nat(payout.amount?),
            terms: payout.terms.into(),
            network_fee: Some(nat(payout.network_fee())),
        })
    }
}

impl TryFrom<PayoutSettings> for domain::PayoutSettings {
    type Error = PoolWireError;

    fn try_from(settings: PayoutSettings) -> Result<Self, Self::Error> {
        Ok(Self {
            min_network_fee: amount("min_network_fee", &settings.min_network_fee)?,
            rebalance_share_floor_bps: settings.rebalance_share_floor_bps,
            order_wait: Duration::from_secs(settings.order_wait_s),
            max_payout_gas: settings.max_payout_gas,
            cctp_min_network_fee: settings
                .cctp_min_network_fee
                .as_ref()
                .map(|fee| amount("cctp_min_network_fee", fee))
                .transpose()?,
        })
    }
}

impl From<&domain::PayoutSettings> for PayoutSettings {
    fn from(settings: &domain::PayoutSettings) -> Self {
        Self {
            min_network_fee: nat(settings.min_network_fee),
            rebalance_share_floor_bps: settings.rebalance_share_floor_bps,
            order_wait_s: settings.order_wait.as_secs(),
            max_payout_gas: settings.max_payout_gas,
            cctp_min_network_fee: settings.cctp_min_network_fee.map(nat),
        }
    }
}
