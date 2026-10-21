//! The canister's own route for a move (spec 3.4: always there, built and signed by the
//! canister itself): one USDC to USDC CCTP burn, its order the canister's (payer the
//! canister, `amountIn` the move's whole amount, NO fee step: the canister pays itself
//! nothing), under the move's own ref. The first order the canister builds; until P11c it only
//! checked the quoter's. The shapes are the quoter's own (the staging canary's orders), so the
//! Runner, the CctpReceiver and the watcher read them as they read every order:
//!
//! - Base to ICP, on Base: a burn to Ethereum minted to and called by Ethereum's
//!   CctpReceiver, its envelope a `CkDeposit` for the canister's OWN principal and its
//!   rebalance subaccount, refunding the canister's own Ethereum address.
//! - ICP to Base, on Ethereum: a burn to Base minted to the canister's own Base address,
//!   received by anyone, its envelope a `Deliver` to the canister of Base's USDC.
//!
//! Standard finality (no Circle fee but the messenger's least) unless urgent; fast offers at
//! most the proposals' fee cap (`cost::max_fee`).

#[cfg(test)]
mod tests;

use alloy_primitives::{FixedBytes, U256};
use candid::Principal;
use types::config::EvmChain;
use types::pool::rebalance::burn_data;
use types::pool::{ck_word, move_ref, rebalance_subaccount, MoveDirection};
use types::swapic::{self, depositForBurnWithHookCall, offsets, Action, Envelope, Order, Payout};
use types::swapic::{Step, Write};
use types::{EvmAddress, TokenAmount};

/// How long the own order may wait to run: a signature, the outbox's broadcast and its
/// replacements at a higher fee.
pub const ORDER_LIFE_S: u64 = 3_600;

/// How long after the order's deadline a CkDeposit may still be completed: the attestation
/// at standard finality (Base's, about 20 minutes) and the finish, with hours of room, so a
/// completion never falls into the release window.
pub const ENVELOPE_LIFE_S: u64 = 6 * 3_600;

/// The gas a CkDeposit's `fill` must have left for the ck helper's deposit: the quoter's own
/// `min_gas_ck_deposit`.
pub const MIN_GAS_CK_DEPOSIT: u64 = 300_000;

/// CCTP v2's `minFinalityThreshold`: fast, and standard.
pub const FAST_FINALITY: u32 = 1_000;
pub const STANDARD_FINALITY: u32 = 2_000;

/// What the own order is built from: where it runs and where it lands, and the canister.
pub struct OwnRoute<'a> {
    pub direction: MoveDirection,
    /// The chain the order runs on.
    pub source: &'a EvmChain,
    /// The chain the burn lands on, and its CCTP domain.
    pub destination: &'a EvmChain,
    pub destination_domain: u32,
    /// The canister's own EVM address, the same on every chain.
    pub canister: EvmAddress,
    /// The canister's own principal, which a Base to ICP move's ck deposit credits.
    pub principal: Principal,
}

fn word(address: EvmAddress) -> FixedBytes<32> {
    FixedBytes(address.to_word())
}

fn u256(amount: TokenAmount) -> U256 {
    U256::from_be_bytes(amount.to_be_bytes())
}

impl OwnRoute<'_> {
    /// The envelope the burn carries to the destination.
    pub fn envelope(
        &self,
        id: u64,
        amount: TokenAmount,
        max_fee: TokenAmount,
        deadline: u64,
    ) -> Envelope {
        let reference = move_ref(id).into_bytes();
        match self.direction {
            MoveDirection::BaseToIcp => Envelope {
                reference,
                action: Action::CkDeposit,
                deadline,
                // a deposit that cannot be made refunds the canister on Ethereum
                recipient: self.canister,
                token_out: EvmAddress::ZERO,
                min_out: U256::ZERO,
                principal: ck_word(&self.principal),
                subaccount: rebalance_subaccount(),
                min_gas: MIN_GAS_CK_DEPOSIT,
            },
            MoveDirection::IcpToBase => Envelope {
                reference,
                action: Action::Deliver,
                deadline,
                recipient: self.canister,
                token_out: self.destination.usdc,
                min_out: u256(amount.checked_sub(max_fee).unwrap_or(TokenAmount::ZERO)),
                principal: [0; 32],
                subaccount: [0; 32],
                min_gas: 0,
            },
        }
    }

    /// The own order for move `id` of `amount`, offering Circle at most `max_fee`, fast or
    /// at standard finality, signed at `now_s`.
    pub fn order(
        &self,
        id: u64,
        amount: TokenAmount,
        max_fee: TokenAmount,
        fast: bool,
        now_s: u64,
    ) -> Order {
        let deadline = now_s.saturating_add(ORDER_LIFE_S);
        let envelope = self.envelope(
            id,
            amount,
            max_fee,
            deadline.saturating_add(ENVELOPE_LIFE_S),
        );
        // a CkDeposit is minted to and called by the destination's CctpReceiver; a Deliver
        // to its recipient, received by anyone
        let (mint_recipient, destination_caller) = match self.direction {
            MoveDirection::BaseToIcp => (
                word(self.destination.cctp_receiver),
                word(self.destination.cctp_receiver),
            ),
            MoveDirection::IcpToBase => (word(self.canister), FixedBytes::ZERO),
        };
        let burn = depositForBurnWithHookCall {
            // the Runner writes the delta here
            amount: U256::ZERO,
            destinationDomain: self.destination_domain,
            mintRecipient: mint_recipient,
            burnToken: swapic::addr(self.source.usdc),
            destinationCaller: destination_caller,
            maxFee: u256(max_fee),
            minFinalityThreshold: if fast {
                FAST_FINALITY
            } else {
                STANDARD_FINALITY
            },
            hookData: envelope.encode().into(),
        };
        Order {
            r#ref: FixedBytes(move_ref(id).into_bytes()),
            payer: swapic::addr(self.canister),
            tokenIn: swapic::addr(self.source.usdc),
            amountIn: u256(amount),
            deadline: U256::from(deadline),
            steps: vec![Step {
                target: swapic::addr(self.source.token_messenger),
                data: burn_data(&burn).into(),
                token: swapic::addr(self.source.usdc),
                // no fee step and no swap: the Runner's delta is the whole amount
                min: u256(amount),
                writes: vec![Write {
                    at: offsets::CCTP_AMOUNT,
                    minus: U256::ZERO,
                }],
            }],
            payout: Payout {
                token: alloy_primitives::Address::ZERO,
                recipient: alloy_primitives::Address::ZERO,
                minOut: U256::ZERO,
            },
        }
    }
}
