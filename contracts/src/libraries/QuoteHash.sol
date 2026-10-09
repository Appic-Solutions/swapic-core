// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import {SwapParams, DexCall, EcoTerms, Delivery} from "../SwapicTypes.sol";

/// THE QUOTE HASH: the EIP-712 digest of the full SwapParams under the
/// router's domain, recomputed on chain from the params the router acts on
/// (spec 4b.5). The 3009 nonce, the Permit2 witness and the QuoteAcceptance
/// all bind it, so an authorization can never be paired with other params.
///
///   quoteHash = keccak256("\x19\x01" ++ domainSeparator ++ hashStruct(SwapParams))
///   domain    = EIP712Domain(name "Swapic Router", version "1", chainId, router)
///
/// Enums are encoded as `uint8`, `bytes` fields as their keccak256, nested
/// structs as their own hashStruct, and the referenced types are appended to
/// the primary type in alphabetical order (Delivery, DexCall, EcoTerms), so
/// viem's `hashTypedData`, alloy's `eip712_signing_hash` and ethers'
/// `TypedDataEncoder.hash` reproduce it from the JSON types in abi.md.
library QuoteHash {
    string internal constant DOMAIN_NAME = "Swapic Router";
    string internal constant DOMAIN_VERSION = "1";

    bytes32 internal constant DOMAIN_TYPEHASH =
        keccak256("EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)");

    string internal constant DEX_CALL_TYPE = "DexCall(address target,bytes data)";
    string internal constant ECO_TERMS_TYPE =
        "EcoTerms(bytes32 salt,uint64 routeDeadline,uint64 rewardDeadline,uint256 amount,uint256 amountOut)";
    string internal constant DELIVERY_TYPE =
        "Delivery(uint8 action,bytes32 recipient,address tokenOut,uint256 minOut,bytes32 principal,bytes32 subaccount,uint64 deadline,DexCall swap)";
    string internal constant SWAP_PARAMS_TYPE =
        "SwapParams(bytes32 ref,address payer,address tokenIn,uint256 amountIn,uint256 deadline,DexCall leg,uint256 legMinOut,uint256 feeBps,uint256 maxGasFee,uint8 rail,uint64 dstChainId,uint256 maxFee,uint32 minFinality,EcoTerms eco,Delivery dst)";

    /// What the payer signs on the 2612 door: the quote's readable economics,
    /// built by the router from the params (never passed in), plus the quote
    /// hash that binds everything else.
    string internal constant QUOTE_ACCEPTANCE_TYPE =
        "QuoteAcceptance(bytes32 quoteHash,address payer,address tokenIn,uint256 amountIn,uint256 deadline,uint64 dstChainId,bytes32 recipient,address tokenOut,uint256 minOut,uint256 maxGasFee)";

    bytes32 internal constant DEX_CALL_TYPEHASH = keccak256(bytes(DEX_CALL_TYPE));
    bytes32 internal constant ECO_TERMS_TYPEHASH = keccak256(bytes(ECO_TERMS_TYPE));
    bytes32 internal constant DELIVERY_TYPEHASH = keccak256(abi.encodePacked(DELIVERY_TYPE, DEX_CALL_TYPE));
    bytes32 internal constant SWAP_PARAMS_TYPEHASH =
        keccak256(abi.encodePacked(SWAP_PARAMS_TYPE, DELIVERY_TYPE, DEX_CALL_TYPE, ECO_TERMS_TYPE));
    bytes32 internal constant QUOTE_ACCEPTANCE_TYPEHASH = keccak256(bytes(QUOTE_ACCEPTANCE_TYPE));

    function domainSeparator(uint256 chainId, address verifyingContract) internal pure returns (bytes32) {
        return keccak256(
            abi.encode(
                DOMAIN_TYPEHASH,
                keccak256(bytes(DOMAIN_NAME)),
                keccak256(bytes(DOMAIN_VERSION)),
                chainId,
                verifyingContract
            )
        );
    }

    function hashDexCall(DexCall memory c) internal pure returns (bytes32) {
        return keccak256(abi.encode(DEX_CALL_TYPEHASH, c.target, keccak256(c.data)));
    }

    function hashEcoTerms(EcoTerms memory e) internal pure returns (bytes32) {
        return
            keccak256(abi.encode(ECO_TERMS_TYPEHASH, e.salt, e.routeDeadline, e.rewardDeadline, e.amount, e.amountOut));
    }

    function hashDelivery(Delivery memory d) internal pure returns (bytes32) {
        return keccak256(
            abi.encode(
                DELIVERY_TYPEHASH,
                uint8(d.action),
                d.recipient,
                d.tokenOut,
                d.minOut,
                d.principal,
                d.subaccount,
                d.deadline,
                hashDexCall(d.swap)
            )
        );
    }

    /// hashStruct(SwapParams). Two abi.encode halves of whole words, which
    /// concatenate to the single encoding EIP-712 specifies.
    function hashParams(SwapParams memory p) internal pure returns (bytes32) {
        bytes memory head = abi.encode(
            SWAP_PARAMS_TYPEHASH, p.ref, p.payer, p.tokenIn, p.amountIn, p.deadline, hashDexCall(p.leg), p.legMinOut
        );
        bytes memory tail = abi.encode(
            p.feeBps,
            p.maxGasFee,
            uint8(p.rail),
            p.dstChainId,
            p.maxFee,
            p.minFinality,
            hashEcoTerms(p.eco),
            hashDelivery(p.dst)
        );
        return keccak256(abi.encodePacked(head, tail));
    }

    function typedDataHash(bytes32 separator, bytes32 structHash) internal pure returns (bytes32 digest) {
        assembly ("memory-safe") {
            let ptr := mload(0x40)
            mstore(ptr, hex"1901")
            mstore(add(ptr, 0x02), separator)
            mstore(add(ptr, 0x22), structHash)
            digest := keccak256(ptr, 0x42)
        }
    }

    /// The quote hash of `p` for the router at `verifyingContract` on `chainId`.
    function quoteHash(SwapParams memory p, uint256 chainId, address verifyingContract)
        internal
        pure
        returns (bytes32)
    {
        return typedDataHash(domainSeparator(chainId, verifyingContract), hashParams(p));
    }

    /// The EIP-712 digest the payer signs on the 2612 door.
    function acceptanceDigest(SwapParams memory p, bytes32 quoteHash_, uint256 chainId, address verifyingContract)
        internal
        pure
        returns (bytes32)
    {
        bytes memory head =
            abi.encode(QUOTE_ACCEPTANCE_TYPEHASH, quoteHash_, p.payer, p.tokenIn, p.amountIn, p.deadline, p.dstChainId);
        bytes memory tail = abi.encode(p.dst.recipient, p.dst.tokenOut, p.dst.minOut, p.maxGasFee);
        return typedDataHash(domainSeparator(chainId, verifyingContract), keccak256(abi.encodePacked(head, tail)));
    }
}
