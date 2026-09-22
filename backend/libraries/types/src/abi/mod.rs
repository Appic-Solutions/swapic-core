//! The calldata of every call this canister sends: the vault's four doors and CCTP v2's
//! two. One builder per call, taking the domain's own types, so no caller assembles a
//! selector or a word by hand.
//!
//! The signatures are declared once, in Solidity, and `alloy-sol-types` derives both the
//! selector and the encoding from them. Each unit test pins the result against
//! `cast calldata`, with the command that produced it above the fixture.

#[cfg(test)]
mod tests;

use crate::evm::EvmAddress;
use crate::hash::QuoteHash;
use crate::numeric::{TokenAmount, UnixSeconds, Wei};
use alloy_primitives::{Address, FixedBytes, I256, U256};
use alloy_sol_types::{sol, SolCall, SolEvent};

sol! {
    /// One router call inside a vault execution.
    struct Call {
        address target;
        uint256 value;
        bytes data;
        address approveToken;
        uint256 approveAmount;
    }

    /// The minimum balance change a vault execution must leave behind.
    struct Delta {
        address token;
        int256 minChange;
    }

    /// What every vault entry door logs: the quote the funds are for, the token, the
    /// payer, and the amount the vault measured as received.
    event Deposited(bytes32 indexed quoteHash, address indexed token, address indexed from, uint256 amount);

    function execute(bytes32 swapRef, Call[] calls, Delta[] deltas);
    function payout(bytes32 swapRef, address token, address to, uint256 amount);
    function refund(bytes32 ref, address token, address to, uint256 amount);
    function pullWithPermit(
        bytes32 quoteHash,
        address token,
        address owner,
        uint256 amount,
        uint256 deadline,
        uint8 v,
        bytes32 r,
        bytes32 s
    );
    function depositForBurn(
        uint256 amount,
        uint32 destinationDomain,
        bytes32 mintRecipient,
        address burnToken,
        bytes32 destinationCaller,
        uint256 maxFee,
        uint32 minFinalityThreshold
    );
    function receiveMessage(bytes message, bytes attestation);
}

/// One router call the vault runs on this canister's behalf: the vault approves
/// `approve_amount` of `approve_token` to `target`, calls it, and zeroes the approval
/// again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VaultCall {
    pub target: EvmAddress,
    pub value: Wei,
    pub data: Vec<u8>,
    pub approve_token: EvmAddress,
    pub approve_amount: TokenAmount,
}

/// The least a token's balance may change over an execution. Signed, because a leg that
/// spends a token declares a floor below zero; held as an `i128`, which covers every
/// amount this canister ever asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VaultDelta {
    pub token: EvmAddress,
    pub min_change: i128,
}

/// An EIP-2612 permit signature, as the token contract takes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Permit {
    pub v: u8,
    pub r: [u8; 32],
    pub s: [u8; 32],
}

/// A CCTP v2 burn. `mint_recipient` and `destination_caller` are 32-byte words because
/// CCTP addresses are not all 20 bytes; [`EvmAddress::to_word`] makes one from an EVM
/// address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Burn {
    pub amount: TokenAmount,
    pub destination_domain: u32,
    pub mint_recipient: [u8; 32],
    pub burn_token: EvmAddress,
    /// The zero word lets anyone deliver the mint.
    pub destination_caller: [u8; 32],
    /// The most of `amount` the fast path may take as its fee.
    pub max_fee: TokenAmount,
    /// 1000 is CCTP v2's fast threshold, 2000 its standard one.
    pub min_finality_threshold: u32,
}

fn address(address: EvmAddress) -> Address {
    Address::from(*address.as_bytes())
}

fn amount<Unit>(amount: crate::checked_amount::CheckedAmountOf<Unit>) -> U256 {
    U256::from_be_bytes(amount.to_be_bytes())
}

fn word(bytes: [u8; 32]) -> FixedBytes<32> {
    FixedBytes(bytes)
}

/// The topic a `Deposited` log carries first: the keccak of
/// `Deposited(bytes32,address,address,uint256)`, which is what the deposit read filters
/// the vault's logs by.
pub fn deposited_topic() -> [u8; 32] {
    Deposited::SIGNATURE_HASH.0
}

/// `execute(bytes32,(address,uint256,bytes,address,uint256)[],(address,int256)[])`.
pub fn vault_execute(swap_ref: QuoteHash, calls: &[VaultCall], deltas: &[VaultDelta]) -> Vec<u8> {
    executeCall {
        swapRef: word(swap_ref.into_bytes()),
        calls: calls
            .iter()
            .map(|call| Call {
                target: address(call.target),
                value: amount(call.value),
                data: call.data.clone().into(),
                approveToken: address(call.approve_token),
                approveAmount: amount(call.approve_amount),
            })
            .collect(),
        deltas: deltas
            .iter()
            .map(|delta| Delta {
                token: address(delta.token),
                minChange: I256::try_from(delta.min_change)
                    .expect("BUG: every i128 is inside an int256"),
            })
            .collect(),
    }
    .abi_encode()
}

/// `payout(bytes32,address,address,uint256)`.
pub fn vault_payout(
    swap_ref: QuoteHash,
    token: EvmAddress,
    to: EvmAddress,
    value: TokenAmount,
) -> Vec<u8> {
    payoutCall {
        swapRef: word(swap_ref.into_bytes()),
        token: address(token),
        to: address(to),
        amount: amount(value),
    }
    .abi_encode()
}

/// `refund(bytes32,address,address,uint256)`.
pub fn vault_refund(
    swap_ref: QuoteHash,
    token: EvmAddress,
    to: EvmAddress,
    value: TokenAmount,
) -> Vec<u8> {
    refundCall {
        r#ref: word(swap_ref.into_bytes()),
        token: address(token),
        to: address(to),
        amount: amount(value),
    }
    .abi_encode()
}

/// `pullWithPermit(bytes32,address,address,uint256,uint256,uint8,bytes32,bytes32)`.
pub fn vault_pull_with_permit(
    quote_hash: QuoteHash,
    token: EvmAddress,
    owner: EvmAddress,
    value: TokenAmount,
    deadline: UnixSeconds,
    permit: &Permit,
) -> Vec<u8> {
    pullWithPermitCall {
        quoteHash: word(quote_hash.into_bytes()),
        token: address(token),
        owner: address(owner),
        amount: amount(value),
        deadline: U256::from(deadline.get()),
        v: permit.v,
        r: word(permit.r),
        s: word(permit.s),
    }
    .abi_encode()
}

/// `depositForBurn(uint256,uint32,bytes32,address,bytes32,uint256,uint32)`.
pub fn cctp_deposit_for_burn(burn: &Burn) -> Vec<u8> {
    depositForBurnCall {
        amount: amount(burn.amount),
        destinationDomain: burn.destination_domain,
        mintRecipient: word(burn.mint_recipient),
        burnToken: address(burn.burn_token),
        destinationCaller: word(burn.destination_caller),
        maxFee: amount(burn.max_fee),
        minFinalityThreshold: burn.min_finality_threshold,
    }
    .abi_encode()
}

/// `receiveMessage(bytes,bytes)`.
pub fn cctp_receive_message(message: &[u8], attestation: &[u8]) -> Vec<u8> {
    receiveMessageCall {
        message: message.to_vec().into(),
        attestation: attestation.to_vec().into(),
    }
    .abi_encode()
}
