//! The SwapicRouter as this canister speaks to it: the params struct, the gasless
//! authorization, the EIP-712 quote hash the router recomputes on chain, and the digests
//! the three gasless doors bind that hash with.
//!
//! The structs are the router's own (`contracts/src/SwapicTypes.sol`), declared once in
//! Solidity so `alloy-sol-types` derives the ABI and the EIP-712 encoding from the same
//! text. Enums are `uint8` on the wire, as the router declares them. The golden test pins
//! the quote hash against the value the contracts' `AbiGolden.t.sol` pins.

#[cfg(test)]
mod tests;

use crate::chain::ChainId;
use crate::evm::EvmAddress;
use crate::hash::QuoteHash;
use alloy_primitives::{Address, FixedBytes, U256};
use alloy_sol_types::{sol, Eip712Domain, SolCall, SolStruct, SolValue};
use std::borrow::Cow;

sol! {
    #![sol(all_derives)]

    struct DexCall { address target; bytes data; }

    struct EcoTerms {
        bytes32 salt;
        uint64 routeDeadline;
        uint64 rewardDeadline;
        uint256 amount;
        uint256 amountOut;
    }

    struct Delivery {
        uint8 action;
        bytes32 recipient;
        address tokenOut;
        uint256 minOut;
        bytes32 principal;
        bytes32 subaccount;
        uint64 deadline;
        DexCall swap;
    }

    struct SwapParams {
        bytes32 ref;
        address payer;
        address tokenIn;
        uint256 amountIn;
        uint256 deadline;
        DexCall leg;
        uint256 legMinOut;
        uint256 feeBps;
        uint256 maxGasFee;
        uint8 rail;
        uint64 dstChainId;
        uint256 maxFee;
        uint32 minFinality;
        EcoTerms eco;
        Delivery dst;
    }

    struct GaslessAuth {
        uint8 door;
        uint256 gasFee;
        uint256 validAfter;
        uint256 validBefore;
        uint256 nonce;
        uint256 deadline;
        bytes signature;
        bytes acceptance;
    }

    /// What the 2612 door's acceptance signs, built by the router from the params.
    struct QuoteAcceptance {
        bytes32 quoteHash;
        address payer;
        address tokenIn;
        uint256 amountIn;
        uint256 deadline;
        uint64 dstChainId;
        bytes32 recipient;
        address tokenOut;
        uint256 minOut;
        uint256 maxGasFee;
    }

    /// USDC's EIP-3009 pull, the 3009 door: the nonce is the quote hash.
    struct ReceiveWithAuthorization {
        address from;
        address to;
        uint256 value;
        uint256 validAfter;
        uint256 validBefore;
        bytes32 nonce;
    }

    struct TokenPermissions { address token; uint256 amount; }
    struct QuoteWitness { bytes32 quoteHash; }

    /// Permit2's witness transfer, the Permit2 door: the witness is the quote hash.
    struct PermitWitnessTransferFrom {
        TokenPermissions permitted;
        address spender;
        uint256 nonce;
        uint256 deadline;
        QuoteWitness witness;
    }

    function swapFor(SwapParams p, GaslessAuth a);
    function paused() returns (bool);
    function usedQuote(bytes32 quoteHash) returns (bool);
    function relayer() returns (address);
    function DOMAIN_SEPARATOR() returns (bytes32);
    event Transfer(address indexed from, address indexed to, uint256 value);
}

/// `Rail` on the wire.
pub const RAIL_CCTP: u8 = 0;
pub const RAIL_ECO: u8 = 1;
/// `Action` on the wire.
pub const ACTION_DELIVER: u8 = 0;
pub const ACTION_SWAP: u8 = 1;
pub const ACTION_CK_DEPOSIT: u8 = 2;
/// `Door` on the wire.
pub const DOOR_AUTH_3009: u8 = 0;
pub const DOOR_PERMIT_2612: u8 = 1;
pub const DOOR_PERMIT2: u8 = 2;

/// Uniswap's Permit2, one address on every chain.
pub const PERMIT2: EvmAddress = EvmAddress::new([
    0x00, 0x00, 0x00, 0x00, 0x00, 0x22, 0xd4, 0x73, 0x03, 0x0f, 0x11, 0x6d, 0xde, 0xe9, 0xf6,
    0xb4, 0x3a, 0xc7, 0x8b, 0xa3,
]);

/// The router's EIP-712 domain on `chain_id` at `router`.
pub fn router_domain(chain_id: ChainId, router: EvmAddress) -> Eip712Domain {
    Eip712Domain {
        name: Some(Cow::Borrowed("Swapic Router")),
        version: Some(Cow::Borrowed("1")),
        chain_id: Some(U256::from(chain_id.get())),
        verifying_contract: Some(addr(router)),
        salt: None,
    }
}

/// The quote hash, as the router recomputes it on chain (spec 4b.5): the full EIP-712
/// digest of the params under the router's domain, never a hash the caller supplies.
pub fn quote_hash(params: &SwapParams, chain_id: ChainId, router: EvmAddress) -> QuoteHash {
    QuoteHash::new(typed_digest(
        router_domain(chain_id, router).separator().0,
        swap_params_hash_struct(params),
    ))
}

/// The router's `SwapParams` type string. Spelled out rather than taken from the macro:
/// `ref` is a Rust keyword, and the macro writes the field into the type string as
/// `r#ref`, which is not the router's type and hashes to another quote.
pub const SWAP_PARAMS_TYPE: &str = "SwapParams(bytes32 ref,address payer,address tokenIn,uint256 amountIn,uint256 deadline,DexCall leg,uint256 legMinOut,uint256 feeBps,uint256 maxGasFee,uint8 rail,uint64 dstChainId,uint256 maxFee,uint32 minFinality,EcoTerms eco,Delivery dst)Delivery(uint8 action,bytes32 recipient,address tokenOut,uint256 minOut,bytes32 principal,bytes32 subaccount,uint64 deadline,DexCall swap)DexCall(address target,bytes data)EcoTerms(bytes32 salt,uint64 routeDeadline,uint64 rewardDeadline,uint256 amount,uint256 amountOut)";

/// `hashStruct(SwapParams)`: the typehash of [`SWAP_PARAMS_TYPE`] and the members as the
/// macro encodes them (the nested structs carry no keyword, so their hashes are right).
pub fn swap_params_hash_struct(params: &SwapParams) -> [u8; 32] {
    let mut preimage = alloy_primitives::keccak256(SWAP_PARAMS_TYPE.as_bytes()).to_vec();
    preimage.extend_from_slice(&params.eip712_encode_data());
    alloy_primitives::keccak256(preimage).0
}

/// The digest the payer's acceptance signs on the 2612 door, under the router's domain.
pub fn acceptance_digest(
    params: &SwapParams,
    quote_hash: QuoteHash,
    chain_id: ChainId,
    router: EvmAddress,
) -> [u8; 32] {
    QuoteAcceptance {
        quoteHash: FixedBytes(quote_hash.into_bytes()),
        payer: params.payer,
        tokenIn: params.tokenIn,
        amountIn: params.amountIn,
        deadline: params.deadline,
        dstChainId: params.dstChainId,
        recipient: params.dst.recipient,
        tokenOut: params.dst.tokenOut,
        minOut: params.dst.minOut,
        maxGasFee: params.maxGasFee,
    }
    .eip712_signing_hash(&router_domain(chain_id, router))
    .0
}

/// The digest an EIP-3009 `receiveWithAuthorization` signs, under the token's own domain
/// separator, which the caller reads off the token.
pub fn receive_with_authorization_digest(
    domain_separator: [u8; 32],
    message: &ReceiveWithAuthorization,
) -> [u8; 32] {
    typed_digest(domain_separator, message.eip712_hash_struct().0)
}

/// The digest Permit2's `permitWitnessTransferFrom` checks, with the quote hash as the
/// witness and the router as the spender.
pub fn permit2_digest(
    chain_id: ChainId,
    params: &SwapParams,
    quote_hash: QuoteHash,
    router: EvmAddress,
    nonce: U256,
    deadline: U256,
) -> [u8; 32] {
    let domain = Eip712Domain {
        name: Some(Cow::Borrowed("Permit2")),
        version: None,
        chain_id: Some(U256::from(chain_id.get())),
        verifying_contract: Some(addr(PERMIT2)),
        salt: None,
    };
    PermitWitnessTransferFrom {
        permitted: TokenPermissions {
            token: params.tokenIn,
            amount: params.amountIn,
        },
        spender: addr(router),
        nonce,
        deadline,
        witness: QuoteWitness {
            quoteHash: FixedBytes(quote_hash.into_bytes()),
        },
    }
    .eip712_signing_hash(&domain)
    .0
}

/// `keccak256(0x1901 ++ separator ++ structHash)`.
pub fn typed_digest(domain_separator: [u8; 32], struct_hash: [u8; 32]) -> [u8; 32] {
    let mut preimage = Vec::with_capacity(66);
    preimage.extend_from_slice(&[0x19, 0x01]);
    preimage.extend_from_slice(&domain_separator);
    preimage.extend_from_slice(&struct_hash);
    alloy_primitives::keccak256(preimage).0
}

/// The params, from their ABI encoding as one tuple (what a client's
/// `encodeAbiParameters` of the `SwapParams` tuple produces).
pub fn decode_params(bytes: &[u8]) -> Option<SwapParams> {
    SwapParams::abi_decode(bytes).ok()
}

/// The params as one ABI tuple, the form a flow stores them in.
pub fn encode_params(params: &SwapParams) -> Vec<u8> {
    params.abi_encode()
}

/// The gasless authorization, from its ABI encoding as one tuple.
pub fn decode_auth(bytes: &[u8]) -> Option<GaslessAuth> {
    GaslessAuth::abi_decode(bytes).ok()
}

/// `swapFor(SwapParams,GaslessAuth)`.
pub fn swap_for_calldata(params: &SwapParams, auth: &GaslessAuth) -> Vec<u8> {
    swapForCall {
        p: params.clone(),
        a: auth.clone(),
    }
    .abi_encode()
}

/// `paused()`.
pub fn paused_calldata() -> Vec<u8> {
    pausedCall {}.abi_encode()
}

/// `usedQuote(bytes32)`.
pub fn used_quote_calldata(quote_hash: QuoteHash) -> Vec<u8> {
    usedQuoteCall {
        quoteHash: FixedBytes(quote_hash.into_bytes()),
    }
    .abi_encode()
}

/// `DOMAIN_SEPARATOR()`.
pub fn domain_separator_calldata() -> Vec<u8> {
    DOMAIN_SEPARATORCall {}.abi_encode()
}

/// The topic an ERC-20 `Transfer` log carries first.
pub fn transfer_topic() -> [u8; 32] {
    use alloy_sol_types::SolEvent;
    Transfer::SIGNATURE_HASH.0
}

/// The router's word for an address.
pub fn addr(address: EvmAddress) -> Address {
    Address::from(*address.as_bytes())
}

/// This canister's address type, from the router's.
pub fn evm(address: Address) -> EvmAddress {
    EvmAddress::new(address.into_array())
}

/// A u256 as a u128, when it fits.
pub fn u128_of(value: U256) -> Option<u128> {
    u128::try_from(value).ok()
}

/// The low 20 bytes of a bytes32 recipient, when its high 12 bytes are zero: an EVM
/// address.
pub fn evm_recipient(recipient: FixedBytes<32>) -> Option<EvmAddress> {
    let bytes = recipient.0;
    if bytes[..12].iter().any(|byte| *byte != 0) {
        return None;
    }
    let mut low = [0_u8; 20];
    low.copy_from_slice(&bytes[12..]);
    Some(EvmAddress::new(low))
}

/// An EVM address as a bytes32 recipient.
pub fn recipient_word(address: EvmAddress) -> FixedBytes<32> {
    FixedBytes(address.to_word())
}
