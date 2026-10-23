// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import {Order, Step, Write, Payout} from "../SwapicTypes.sol";

/// The order fingerprint: the EIP-712 digest of the whole Order under the
/// domain {name "Swapic", version "3", chainId, verifyingContract = Swapic}.
/// Referenced types are appended alphabetically, `bytes` hashed, arrays
/// hashed as keccak of the concatenated struct hashes, so viem's
/// `hashTypedData` and alloy's `eip712_signing_hash` reproduce it.
library OrderHash {
    string internal constant NAME = "Swapic";
    string internal constant VERSION = "3";

    bytes32 internal constant DOMAIN_TYPEHASH =
        keccak256("EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)");

    string internal constant ORDER_TYPE =
        "Order(bytes32 ref,address payer,address tokenIn,uint256 amountIn,uint256 deadline,Step[] steps,Payout payout)";
    string internal constant PAYOUT_TYPE = "Payout(address token,address recipient,uint256 minOut)";
    string internal constant STEP_TYPE = "Step(address target,bytes data,address token,uint256 min,Write[] writes)";
    string internal constant WRITE_TYPE = "Write(uint32 at,uint256 minus)";
    string internal constant ACCEPTANCE_TYPE =
        "QuoteAcceptance(bytes32 orderId,address payer,address tokenIn,uint256 amountIn,uint256 deadline)";

    bytes32 internal constant ORDER_TYPEHASH =
        keccak256(abi.encodePacked(ORDER_TYPE, PAYOUT_TYPE, STEP_TYPE, WRITE_TYPE));
    bytes32 internal constant PAYOUT_TYPEHASH = keccak256(bytes(PAYOUT_TYPE));
    bytes32 internal constant STEP_TYPEHASH = keccak256(abi.encodePacked(STEP_TYPE, WRITE_TYPE));
    bytes32 internal constant WRITE_TYPEHASH = keccak256(bytes(WRITE_TYPE));
    bytes32 internal constant ACCEPTANCE_TYPEHASH = keccak256(bytes(ACCEPTANCE_TYPE));

    function domainSeparator(uint256 chainId, address swapic) internal pure returns (bytes32) {
        return keccak256(abi.encode(DOMAIN_TYPEHASH, keccak256(bytes(NAME)), keccak256(bytes(VERSION)), chainId, swapic));
    }

    function hashWrite(Write calldata w) internal pure returns (bytes32) {
        return keccak256(abi.encode(WRITE_TYPEHASH, w.at, w.minus));
    }

    function hashStep(Step calldata s) internal pure returns (bytes32) {
        uint256 n = s.writes.length;
        bytes32[] memory ws = new bytes32[](n);
        for (uint256 i; i < n; ++i) {
            ws[i] = hashWrite(s.writes[i]);
        }
        return keccak256(
            abi.encode(STEP_TYPEHASH, s.target, keccak256(s.data), s.token, s.min, keccak256(abi.encodePacked(ws)))
        );
    }

    function hashPayout(Payout calldata p) internal pure returns (bytes32) {
        return keccak256(abi.encode(PAYOUT_TYPEHASH, p.token, p.recipient, p.minOut));
    }

    function hashOrder(Order calldata o) internal pure returns (bytes32) {
        uint256 n = o.steps.length;
        bytes32[] memory ss = new bytes32[](n);
        for (uint256 i; i < n; ++i) {
            ss[i] = hashStep(o.steps[i]);
        }
        return keccak256(
            abi.encode(
                ORDER_TYPEHASH,
                o.ref,
                o.payer,
                o.tokenIn,
                o.amountIn,
                o.deadline,
                keccak256(abi.encodePacked(ss)),
                hashPayout(o.payout)
            )
        );
    }

    function digest(bytes32 separator, bytes32 structHash) internal pure returns (bytes32 d) {
        assembly ("memory-safe") {
            let ptr := mload(0x40)
            mstore(ptr, hex"1901")
            mstore(add(ptr, 0x02), separator)
            mstore(add(ptr, 0x22), structHash)
            d := keccak256(ptr, 0x42)
        }
    }

    function orderId(Order calldata o, uint256 chainId, address swapic) internal pure returns (bytes32) {
        return digest(domainSeparator(chainId, swapic), hashOrder(o));
    }

    /// What the payer signs on the 2612 door.
    function acceptanceDigest(Order calldata o, bytes32 id, uint256 chainId, address swapic)
        internal
        pure
        returns (bytes32)
    {
        return digest(
            domainSeparator(chainId, swapic),
            keccak256(abi.encode(ACCEPTANCE_TYPEHASH, id, o.payer, o.tokenIn, o.amountIn, o.deadline))
        );
    }
}
