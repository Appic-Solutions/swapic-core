//! The controller's sweeps: what is the canister's own and never the pool's, sent to an
//! address the controller names.

use crate::types::errors::GuardError;
use crate::types::pool::IcpAccount;
use candid::{CandidType, Nat};
use serde::Deserialize;
use types::sweep as domain;

/// What `sweep_evm` sends from the canister's address on an EVM chain.
#[derive(CandidType, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvmSweepAsset {
    /// The chain's native gas, in wei.
    Native,
    /// USDC fee income booked on the chain, in micro USDC.
    FeeUsdc,
}

/// What `sweep_icp` sends from the canister's ICP accounts.
#[derive(CandidType, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum IcpSweepAsset {
    /// The pool's ICP fee line, in micro ckUSDC.
    CkUsdcFees,
    /// The canister's ckETH, in wei.
    CkEth,
    /// The canister's ICP, in e8s.
    Icp,
}

#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SweepEvmArgs {
    pub chain_id: u64,
    pub asset: EvmSweepAsset,
    /// In the asset's smallest unit.
    pub amount: Nat,
    /// An EVM address: never the canister's own, the zero address or a sanctioned one.
    pub to: String,
}

#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SweepIcpArgs {
    pub asset: IcpSweepAsset,
    /// In the asset's smallest unit; the ledger's fee is paid on top, from the same account.
    pub amount: Nat,
    /// Never the canister's own account.
    pub to: IcpAccount,
}

/// `sweep_evm`'s answer: the sweep's id, and its signed transaction's id in `signed_txs`.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SweepEvmOk {
    pub id: u64,
    pub tx_id: u64,
}

/// `sweep_icp`'s answer: the sweep's id, and the ledger block of its transfer.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SweepIcpOk {
    pub id: u64,
    pub block: Nat,
}

/// Why a sweep sent nothing.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum SweepError {
    Guard(GuardError),
    /// An argument the door does not take: the field and why.
    Invalid(String),
    /// More than the books allow: what they allow now.
    AboveBooks {
        available: Nat,
    },
    /// Open work may still need the asset: what is open.
    Busy(String),
    /// The destination is sanctioned.
    Sanctioned,
    /// A chain or a ledger did not answer usably: ask again.
    Unavailable(String),
    /// The ledger refused the transfer: why. Nothing left the books.
    Ledger(String),
}

/// What a logged sweep sent.
#[derive(CandidType, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum SweepAsset {
    Native,
    FeeUsdc,
    CkUsdcFees,
    CkEth,
    Icp,
}

/// Where a logged sweep sent to.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum SweepTarget {
    /// An EIP-55 address on `chain_id`.
    Evm {
        chain_id: u64,
        to: String,
    },
    Icp(IcpAccount),
}

impl From<domain::SweepAsset> for SweepAsset {
    fn from(asset: domain::SweepAsset) -> Self {
        match asset {
            domain::SweepAsset::Native => Self::Native,
            domain::SweepAsset::FeeUsdc => Self::FeeUsdc,
            domain::SweepAsset::CkUsdcFees => Self::CkUsdcFees,
            domain::SweepAsset::CkEth => Self::CkEth,
            domain::SweepAsset::Icp => Self::Icp,
        }
    }
}

impl From<SweepAsset> for domain::SweepAsset {
    fn from(asset: SweepAsset) -> Self {
        match asset {
            SweepAsset::Native => Self::Native,
            SweepAsset::FeeUsdc => Self::FeeUsdc,
            SweepAsset::CkUsdcFees => Self::CkUsdcFees,
            SweepAsset::CkEth => Self::CkEth,
            SweepAsset::Icp => Self::Icp,
        }
    }
}

impl From<EvmSweepAsset> for domain::SweepAsset {
    fn from(asset: EvmSweepAsset) -> Self {
        match asset {
            EvmSweepAsset::Native => Self::Native,
            EvmSweepAsset::FeeUsdc => Self::FeeUsdc,
        }
    }
}

impl From<IcpSweepAsset> for domain::SweepAsset {
    fn from(asset: IcpSweepAsset) -> Self {
        match asset {
            IcpSweepAsset::CkUsdcFees => Self::CkUsdcFees,
            IcpSweepAsset::CkEth => Self::CkEth,
            IcpSweepAsset::Icp => Self::Icp,
        }
    }
}

impl From<domain::SweepTo> for SweepTarget {
    fn from(to: domain::SweepTo) -> Self {
        match to {
            domain::SweepTo::Evm { chain, to } => Self::Evm {
                chain_id: chain.get(),
                to: to.to_string(),
            },
            domain::SweepTo::Icp(account) => Self::Icp(account.into()),
        }
    }
}
