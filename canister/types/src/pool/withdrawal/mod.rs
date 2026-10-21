//! An EVM staker's withdrawal request and the EIP-712 message the staker signs for it (the
//! fixed interface section 3b): the frontend and the quoter's withdraw quote build exactly
//! this, and `withdraw_evm` recovers the staker from it.
//!
//! - Domain: `name = "Swapic Pool"`, `version = "1"`, the pool chain's id, and Swapic on it as
//!   the verifying contract (read from the canister's config, never hard coded).
//! - Type: [`WITHDRAWAL_TYPE`], its `deadline` the payout's deadline in whole seconds.

#[cfg(test)]
mod tests;

use super::payout::PayoutTerms;
use super::Shares;
use crate::chain::ChainId;
use crate::evm::EvmAddress;
use crate::numeric::TokenAmount;
use crate::swapic::addr;
use alloy_primitives::U256;
use alloy_sol_types::{sol, Eip712Domain, SolStruct};
use std::borrow::Cow;

sol! {
    /// The message an EVM staker signs to withdraw.
    struct Withdrawal {
        address staker;
        uint256 usdc;
        uint256 maxShares;
        uint256 rebalanceShare;
        uint256 networkFee;
        uint64 dstChain;
        address recipient;
        address tokenOut;
        uint256 minOut;
        uint64 deadline;
        uint64 nonce;
    }
}

pub const WITHDRAWAL_TYPE: &str = "Withdrawal(address staker,uint256 usdc,uint256 maxShares,\
uint256 rebalanceShare,uint256 networkFee,uint64 dstChain,address recipient,address tokenOut,\
uint256 minOut,uint64 deadline,uint64 nonce)";

pub const DOMAIN_NAME: &str = "Swapic Pool";
pub const DOMAIN_VERSION: &str = "1";

/// A withdrawal as the staker asks it: every field it signs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WithdrawalRequest {
    pub staker: EvmAddress,
    pub usdc: TokenAmount,
    pub max_shares: Shares,
    pub rebalance_share: TokenAmount,
    pub network_fee: TokenAmount,
    pub payout: PayoutTerms,
    pub nonce: u64,
}

fn uint(amount: TokenAmount) -> U256 {
    U256::from_be_bytes(amount.to_be_bytes())
}

/// The pool's EIP-712 domain on `chain_id`, Swapic there its verifying contract.
pub fn domain(chain_id: ChainId, swapic: EvmAddress) -> Eip712Domain {
    Eip712Domain {
        name: Some(Cow::Borrowed(DOMAIN_NAME)),
        version: Some(Cow::Borrowed(DOMAIN_VERSION)),
        chain_id: Some(U256::from(chain_id.get())),
        verifying_contract: Some(addr(swapic)),
        salt: None,
    }
}

impl WithdrawalRequest {
    /// The message, the deadline in whole seconds (floor).
    pub fn message(&self) -> Withdrawal {
        Withdrawal {
            staker: addr(self.staker),
            usdc: uint(self.usdc),
            maxShares: U256::from(self.max_shares.get()),
            rebalanceShare: uint(self.rebalance_share),
            networkFee: uint(self.network_fee),
            dstChain: self.payout.dst_chain.get(),
            recipient: addr(self.payout.recipient),
            tokenOut: addr(self.payout.token_out),
            minOut: uint(self.payout.min_out),
            deadline: self.payout.deadline.as_secs().get(),
            nonce: self.nonce,
        }
    }

    /// The digest the staker signs: `keccak256(0x1901 ‖ domainSeparator ‖ hashStruct)`.
    pub fn digest(&self, chain_id: ChainId, swapic: EvmAddress) -> [u8; 32] {
        self.message()
            .eip712_signing_hash(&domain(chain_id, swapic))
            .0
    }
}
