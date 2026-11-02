//! A controller's sweep of what is the canister's own and never the pool's: native gas or
//! booked USDC fee income on an EVM chain, ckUSDC fee income, ckETH or ICP on ICP.

use crate::chain::ChainId;
use crate::evm::EvmAddress;
use crate::numeric::TokenAmount;
use crate::pool::IcpAccount;
use minicbor::{Decode, Encode};

/// What a sweep sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(index_only)]
pub enum SweepAsset {
    /// The canister's native gas on an EVM chain, in wei.
    #[n(0)]
    Native,
    /// USDC fee income booked on an EVM chain, in micro USDC: the chain books' fee income
    /// and, on the pool chain, the pool's Base fee line.
    #[n(1)]
    FeeUsdc,
    /// The pool's ICP fee line, in micro ckUSDC.
    #[n(2)]
    CkUsdcFees,
    /// The canister's ckETH, in wei.
    #[n(3)]
    CkEth,
    /// The canister's ICP, in e8s.
    #[n(4)]
    Icp,
}

impl SweepAsset {
    /// Whether the asset is sent on an EVM chain.
    pub fn is_evm(self) -> bool {
        matches!(self, SweepAsset::Native | SweepAsset::FeeUsdc)
    }
}

/// Where a sweep sends to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub enum SweepTo {
    #[n(0)]
    Evm {
        #[n(0)]
        chain: ChainId,
        #[n(1)]
        to: EvmAddress,
    },
    #[n(1)]
    Icp(#[n(0)] IcpAccount),
}

/// One sweep, as the log records it when it is signed (EVM) or asked of the ledger (ICP).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct Sweep {
    /// The sweep's own number, in order from zero.
    #[n(0)]
    pub id: u64,
    #[n(1)]
    pub asset: SweepAsset,
    #[n(2)]
    pub to: SweepTo,
    /// In the asset's smallest unit.
    #[n(3)]
    pub amount: TokenAmount,
}
