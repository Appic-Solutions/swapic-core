//! The pool's candid types: interface section 3 (`2026-10-19-pool-interface.md`), field for
//! field, and their conversions to and from the domain (`types::pool`) at the boundary.
//! Amounts are micro USDC unless a field names a token.

#[cfg(test)]
mod tests;

pub mod payout;
pub mod rebalance;
pub mod routes;

pub use rebalance::{MoveDirection, MoveResolution, MoveStatus, MoveView, RebalanceSettings};
pub use routes::{IcpRoute, IcpRouteVia, IcpRoutesView};

pub use payout::{
    EvmWithdrawal, IcpSwapTerms, PayoutKind, PayoutPlan, PayoutResolution, PayoutSettings,
    PayoutStatus, PayoutTerms, PayoutView, PayoutWaiting,
};

use candid::{CandidType, Nat, Principal};
use serde::Deserialize;
use std::time::Duration;
use thiserror::Error;
use types::hash::PoolRef;
use types::pool::{self as domain, RegisterRefusal};
use types::{EvmAddress, TokenAmount};

/// Who owns shares.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum PoolAccount {
    /// An EIP-55 or lowercase 0x address.
    Evm(String),
    Icp(IcpAccount),
}

#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct IcpAccount {
    pub owner: Principal,
    pub subaccount: Option<Vec<u8>>,
}

#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum PoolDepositKind {
    /// `token_out`: a ledger in `pool_settings.icp_tokens`, ckUSDC itself included.
    Swap {
        recipient: IcpAccount,
        token_out: Principal,
        min_out: Nat,
    },
    Stake {
        owner: PoolAccount,
    },
}

#[derive(CandidType, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum PoolRail {
    Cctp,
    Eco,
    SameChain,
}

/// A pool deposit's terms (P11d, 2026-10-21: no registration): the preimage of the ref its
/// order carries (section 2), handed to the canister with the arrival. Every fee is the
/// user's own line: a swap's premium (pure pool income) and its ICP path's ledger fees
/// (`icp_fees`, at least the path's own ckUSDC fees); a stake's rebalancing share.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PoolTerms {
    pub kind: PoolDepositKind,
    /// Swap: micro USDC; Stake: 0.
    pub premium: Nat,
    /// Swap: micro USDC; Stake: 0.
    pub icp_fees: Nat,
    /// Stake: micro USDC; Swap: 0.
    pub rebalance_share: Nat,
    /// 32 bytes.
    pub salt: Vec<u8>,
}

#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum PoolError {
    /// `require_pool_open`'s reason.
    Closed(String),
    Halted,
    /// The role guard, or an argument the canister does not take.
    NotAllowed(String),
    /// The ref does not match section 2.
    BadRef,
    /// The ref is registered already.
    Known,
    /// No registration under this ref.
    Unknown,
    Expired,
    AboveSwapCap {
        cap: Nat,
    },
    AboveTvlCap {
        cap: Nat,
        would_be: Nat,
    },
    PremiumTooLow {
        least: Nat,
    },
    TokenNotListed(Principal),
    Sanctioned(String),
    /// The proof failed: why.
    BadArrival(String),
    NotDeep {
        needed: u64,
        have: u64,
    },
    AlreadyCredited,
    ShortShares {
        have: Nat,
        need: Nat,
    },
    /// An ICRC or ICPSwap call failed.
    Ledger(String),
    BadSettings(String),
    /// An outcall failed: ask again.
    Unavailable(String),
}

#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PoolArrival {
    pub base_tx: Vec<u8>,
    pub src_chain: u64,
    pub src_tx: Vec<u8>,
    /// The terms of the deposit the transfer pays, whose ref its proof must carry.
    pub terms: PoolTerms,
}

#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum PoolArrivalOutcome {
    /// The payout follows on the canister's timer.
    Credited {
        r#ref: Vec<u8>,
        amount: Nat,
    },
    Unattributed {
        amount: Nat,
        why: String,
    },
}

#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum PoolDepositStatus {
    Registered,
    Arrived,
    Paying,
    Paid,
    Unattributed,
    Expired,
    PayoutFailed(String),
}

#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PoolDepositView {
    pub r#ref: Vec<u8>,
    pub kind: PoolDepositKind,
    pub min_usdc: Nat,
    pub premium: Nat,
    pub rebalance_share: Nat,
    pub src_chain: u64,
    pub rail: PoolRail,
    pub status: PoolDepositStatus,
    pub arrived: Option<Nat>,
    pub base_tx: Option<Vec<u8>>,
    pub paid: Option<Nat>,
    pub payout_block: Option<u64>,
    pub shares_minted: Option<Nat>,
}

#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PocketView {
    pub free: Nat,
    pub reserved: Nat,
    pub in_flight: Nat,
}

#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PoolStateView {
    pub open: bool,
    pub closed_reason: Option<String>,
    pub base: PocketView,
    pub icp: PocketView,
    pub total_assets: Nat,
    pub total_shares: Nat,
    pub tvl_cap: Nat,
    pub per_swap_cap: Nat,
    pub premium_bps: u16,
}

#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PoolAccountView {
    pub shares: Nat,
    pub value_usdc: Nat,
}

#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct IcpTokenEntry {
    pub ledger: Principal,
    pub icpswap_pool: Principal,
}

#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PoolSettings {
    /// Micro USDC, launch 10_000_000_000 ($10,000).
    pub tvl_cap: Nat,
    /// Micro USDC, launch 1_000_000_000 ($1,000).
    pub per_swap_cap: Nat,
    /// Launch 10.
    pub premium_bps: u16,
    pub ckusdc_ledger: Principal,
    pub icp_tokens: Vec<IcpTokenEntry>,
    /// The canister's own Base read, its backup of the reports (default 300). (P11d drops
    /// the registrations' knobs: `deposit_grace_s`, `max_open_per_payer`, `max_held_bps`.)
    pub arrival_backup_s: u64,
    /// Off until a controller opens the pool for swaps.
    pub pool_open_for_swaps: bool,
    /// Micro USDC: a deposit whose one transfer is above this waits
    /// `large_deposit_confirmations` blocks before it is credited. Absent: 1_000_000_000
    /// ($1,000).
    pub large_deposit_usdc: Option<Nat>,
    /// The pool chain's blocks a large deposit waits. Absent: 6.
    pub large_deposit_confirmations: Option<u64>,
}

#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct StakeIcpArgs {
    pub from_subaccount: Option<Vec<u8>>,
    pub amount: Nat,
    pub owner: PoolAccount,
}

#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct StakeIcpOk {
    pub shares: Nat,
}

#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct WithdrawIcpArgs {
    /// The caller's subaccount owning the shares.
    pub from: Option<Vec<u8>>,
    pub usdc: Nat,
    pub max_shares: Nat,
    pub token_out: Principal,
    pub min_out: Nat,
    pub to: IcpAccount,
}

/// `swap_from_icp`'s answer: the payout's id, and where it stands as the call returns
/// (`Signed` when its plan fitted; `Waiting` when it waits for an order).
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SwapFromIcpOk {
    pub id: u64,
    pub status: PayoutStatus,
}

/// `withdraw_evm`'s answer: the payout's id and the shares burned.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct WithdrawEvmOk {
    pub id: u64,
    pub burned: Nat,
}

/// `pay_payout`'s answer: the signed transaction's id in `signed_txs`.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PayPayoutOk {
    pub tx_id: u64,
}

/// An orphan held for a controller: its credit key (48 bytes: chain, transaction, log
/// index), what arrived, the block, its payer where the chain showed it, and why no deposit
/// took it.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct OrphanView {
    pub key: Vec<u8>,
    pub amount: Nat,
    pub block: u64,
    pub payer: Option<String>,
    pub why: String,
}

/// What a controller does with an orphan.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum OrphanAction {
    /// Sent back on Base, less the least network fee.
    Refund { to: String },
    /// Booked to the Base books' unattributed line.
    Book,
}

/// `pool_approve_swapic`'s answer: the approval's id in `signed_txs`.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PoolApproveOk {
    pub tx_id: u64,
}

/// `rescue_eco_intent`'s answer: the signed `cancelAndProve`'s id in `signed_txs` (P11e).
#[derive(CandidType, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct EcoRescueOk {
    pub tx_id: u64,
}

#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct WithdrawIcpOk {
    pub burned: Nat,
    pub paid: Nat,
}

/// Why a wire value is not a domain one, naming the field.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum PoolWireError {
    #[error("{field} must be 32 bytes, not {len}")]
    NotThirtyTwo { field: &'static str, len: usize },
    #[error("{field} is not an EVM address")]
    NotAnAddress { field: &'static str },
    #[error("{field} does not fit 128 bits")]
    TooLarge { field: &'static str },
}

impl From<PoolWireError> for PoolError {
    fn from(error: PoolWireError) -> Self {
        PoolError::NotAllowed(error.to_string())
    }
}

/// Exactly 32 bytes.
pub fn bytes32(field: &'static str, bytes: &[u8]) -> Result<[u8; 32], PoolWireError> {
    bytes.try_into().map_err(|_| PoolWireError::NotThirtyTwo {
        field,
        len: bytes.len(),
    })
}

/// An amount no wider than 128 bits, the width the log's preimage writes.
pub fn amount(field: &'static str, value: &Nat) -> Result<TokenAmount, PoolWireError> {
    TokenAmount::from_canonical_nat(value.clone()).ok_or(PoolWireError::TooLarge { field })
}

/// A count of some token no wider than 128 bits.
pub fn u128_of(field: &'static str, value: &Nat) -> Result<u128, PoolWireError> {
    u128::try_from(value.0.clone()).map_err(|_| PoolWireError::TooLarge { field })
}

pub fn nat(amount: TokenAmount) -> Nat {
    Nat::from(amount)
}

/// An EIP-55 or single-case address; a mixed case that is no checksum is refused.
pub fn evm_address(field: &'static str, text: &str) -> Result<EvmAddress, PoolWireError> {
    text.parse()
        .map_err(|_| PoolWireError::NotAnAddress { field })
}

impl TryFrom<IcpAccount> for domain::IcpAccount {
    type Error = PoolWireError;

    fn try_from(account: IcpAccount) -> Result<Self, Self::Error> {
        Ok(Self {
            owner: account.owner,
            subaccount: account
                .subaccount
                .map(|bytes| bytes32("subaccount", &bytes))
                .transpose()?,
        })
    }
}

impl From<domain::IcpAccount> for IcpAccount {
    fn from(account: domain::IcpAccount) -> Self {
        Self {
            owner: account.owner,
            subaccount: account.subaccount.map(|bytes| bytes.to_vec()),
        }
    }
}

impl TryFrom<PoolAccount> for domain::PoolAccount {
    type Error = PoolWireError;

    fn try_from(account: PoolAccount) -> Result<Self, Self::Error> {
        Ok(match account {
            PoolAccount::Evm(text) => Self::Evm(evm_address("owner", &text)?),
            PoolAccount::Icp(account) => Self::Icp(account.try_into()?),
        })
    }
}

impl From<domain::PoolAccount> for PoolAccount {
    fn from(account: domain::PoolAccount) -> Self {
        match account {
            domain::PoolAccount::Evm(address) => Self::Evm(address.to_string()),
            domain::PoolAccount::Icp(account) => Self::Icp(account.into()),
        }
    }
}

impl TryFrom<PoolDepositKind> for domain::DepositKind {
    type Error = PoolWireError;

    fn try_from(kind: PoolDepositKind) -> Result<Self, Self::Error> {
        Ok(match kind {
            PoolDepositKind::Swap {
                recipient,
                token_out,
                min_out,
            } => Self::Swap {
                recipient: recipient.try_into()?,
                token_out,
                min_out: u128_of("min_out", &min_out)?,
            },
            PoolDepositKind::Stake { owner } => Self::Stake {
                owner: owner.try_into()?,
            },
        })
    }
}

impl From<domain::DepositKind> for PoolDepositKind {
    fn from(kind: domain::DepositKind) -> Self {
        match kind {
            domain::DepositKind::Swap {
                recipient,
                token_out,
                min_out,
            } => Self::Swap {
                recipient: recipient.into(),
                token_out,
                min_out: Nat::from(min_out),
            },
            domain::DepositKind::Stake { owner } => Self::Stake {
                owner: owner.into(),
            },
        }
    }
}

impl From<PoolRail> for domain::PoolRail {
    fn from(rail: PoolRail) -> Self {
        match rail {
            PoolRail::Cctp => Self::Cctp,
            PoolRail::Eco => Self::Eco,
            PoolRail::SameChain => Self::SameChain,
        }
    }
}

impl From<domain::PoolRail> for PoolRail {
    fn from(rail: domain::PoolRail) -> Self {
        match rail {
            domain::PoolRail::Cctp => Self::Cctp,
            domain::PoolRail::Eco => Self::Eco,
            domain::PoolRail::SameChain => Self::SameChain,
        }
    }
}

impl PoolTerms {
    /// The terms as the domain reads them.
    pub fn into_domain(self) -> Result<domain::DepositTerms, PoolWireError> {
        Ok(domain::DepositTerms {
            kind: self.kind.try_into()?,
            premium: amount("premium", &self.premium)?,
            icp_fees: amount("icp_fees", &self.icp_fees)?,
            rebalance_share: amount("rebalance_share", &self.rebalance_share)?,
            salt: bytes32("salt", &self.salt)?,
        })
    }
}

impl From<domain::DepositStatus> for PoolDepositStatus {
    fn from(status: domain::DepositStatus) -> Self {
        match status {
            domain::DepositStatus::Registered => Self::Registered,
            domain::DepositStatus::Arrived => Self::Arrived,
            domain::DepositStatus::Paying => Self::Paying,
            domain::DepositStatus::Paid => Self::Paid,
            domain::DepositStatus::Unattributed => Self::Unattributed,
            domain::DepositStatus::Expired => Self::Expired,
            domain::DepositStatus::PayoutFailed(why) => Self::PayoutFailed(why),
        }
    }
}

impl PoolDepositView {
    pub fn of(r#ref: PoolRef, deposit: domain::Registered) -> Self {
        let terms = deposit.terms;
        Self {
            r#ref: r#ref.into_bytes().to_vec(),
            kind: terms.kind.into(),
            min_usdc: nat(terms.min_usdc),
            premium: nat(terms.premium),
            rebalance_share: nat(terms.rebalance_share),
            src_chain: terms.src_chain.get(),
            rail: terms.rail.into(),
            status: deposit.status.into(),
            arrived: deposit.arrived.map(|arrival| nat(arrival.amount)),
            base_tx: deposit
                .arrived
                .map(|arrival| arrival.credit.tx.into_bytes().to_vec()),
            paid: deposit.paid.map(|paid| nat(paid.amount)),
            payout_block: deposit.paid.map(|paid| paid.block),
            shares_minted: deposit.minted.map(|shares| Nat::from(shares.get())),
        }
    }
}

impl From<domain::Pocket> for PocketView {
    fn from(pocket: domain::Pocket) -> Self {
        Self {
            free: nat(pocket.free),
            reserved: nat(pocket.reserved),
            in_flight: nat(pocket.in_flight),
        }
    }
}

impl TryFrom<PoolSettings> for domain::PoolSettings {
    type Error = PoolWireError;

    /// The knobs, with no ledger fee read yet: `set_pool_settings` reads them.
    fn try_from(settings: PoolSettings) -> Result<Self, Self::Error> {
        Ok(Self {
            tvl_cap: amount("tvl_cap", &settings.tvl_cap)?,
            per_swap_cap: amount("per_swap_cap", &settings.per_swap_cap)?,
            premium_bps: settings.premium_bps,
            ckusdc_ledger: settings.ckusdc_ledger,
            icp_tokens: settings
                .icp_tokens
                .into_iter()
                .map(|token| domain::IcpToken {
                    ledger: token.ledger,
                    icpswap_pool: token.icpswap_pool,
                })
                .collect(),
            arrival_backup: Duration::from_secs(settings.arrival_backup_s),
            open_for_swaps: settings.pool_open_for_swaps,
            ledger_fees: Vec::new(),
            // the routes are `set_icp_routes`'s, kept across a change of the knobs
            routes: None,
            large_deposit_usdc: settings
                .large_deposit_usdc
                .as_ref()
                .map(|usdc| amount("large_deposit_usdc", usdc))
                .transpose()?,
            large_deposit_confirmations: settings.large_deposit_confirmations,
            // no registration (P11d): kept in the record at their defaults, read by nothing
            ..domain::PoolSettings::default()
        })
    }
}

impl From<&domain::PoolSettings> for PoolSettings {
    fn from(settings: &domain::PoolSettings) -> Self {
        Self {
            tvl_cap: nat(settings.tvl_cap),
            per_swap_cap: nat(settings.per_swap_cap),
            premium_bps: settings.premium_bps,
            ckusdc_ledger: settings.ckusdc_ledger,
            icp_tokens: settings
                .icp_tokens
                .iter()
                .map(|token| IcpTokenEntry {
                    ledger: token.ledger,
                    icpswap_pool: token.icpswap_pool,
                })
                .collect(),
            arrival_backup_s: settings.arrival_backup.as_secs(),
            pool_open_for_swaps: settings.open_for_swaps,
            large_deposit_usdc: Some(nat(settings.large_deposit().0)),
            large_deposit_confirmations: Some(settings.large_deposit().1.get()),
        }
    }
}

impl From<RegisterRefusal> for PoolError {
    fn from(refusal: RegisterRefusal) -> Self {
        match refusal {
            RegisterRefusal::NoSettings => Self::Closed("the pool has no settings".to_string()),
            RegisterRefusal::ClosedForSwaps => {
                Self::Closed("the pool is not open for swaps".to_string())
            }
            RegisterRefusal::PocketShort => Self::Closed("the ICP pocket is short".to_string()),
            RegisterRefusal::BadRef => Self::BadRef,
            RegisterRefusal::Known => Self::Known,
            RegisterRefusal::NotAllowed(why) => Self::NotAllowed(why),
            RegisterRefusal::AboveSwapCap { cap } => Self::AboveSwapCap { cap: nat(cap) },
            RegisterRefusal::AboveTvlCap { cap, would_be } => Self::AboveTvlCap {
                cap: nat(cap),
                would_be: nat(would_be),
            },
            RegisterRefusal::PremiumTooLow { least } => Self::PremiumTooLow { least: nat(least) },
            RegisterRefusal::TokenNotListed(token) => Self::TokenNotListed(token),
        }
    }
}
