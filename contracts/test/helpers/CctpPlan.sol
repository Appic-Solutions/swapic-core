// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import {Step, Write} from "../../src/SwapicTypes.sol";
/// CCTP v2 TokenMessengerV2 (`0x28b5a0e9C621a5BadaA536219b3a228C8168cf5d`),
/// the burn the quoter writes as a step.
interface ITokenMessengerV2 {
    /// selector 0x779b432d
    function depositForBurnWithHook(
        uint256 amount,
        uint32 destinationDomain,
        bytes32 mintRecipient,
        address burnToken,
        bytes32 destinationCaller,
        uint256 maxFee,
        uint32 minFinalityThreshold,
        bytes calldata hookData
    ) external;
}

/// What the quoter writes for CCTP, and the CCTP v2 message the destination
/// receives (Circle evm-cctp-contracts: header 148 bytes, burn body 228 bytes,
/// then the hook data).
library CctpPlan {
    address internal constant TOKEN_MESSENGER = 0x28b5a0e9C621a5BadaA536219b3a228C8168cf5d;

    /// `depositForBurnWithHook(amount, ...)` with the amount written at byte 4:
    /// the burn is always the Runner's whole delta of USDC.
    function burnStep(
        address messenger,
        address usdc,
        uint256 min,
        uint32 domain,
        address mintRecipient,
        address caller,
        uint256 maxFee,
        uint32 finality,
        bytes memory envelope
    ) internal pure returns (Step memory) {
        Write[] memory ws = new Write[](1);
        ws[0] = Write(4, 0);
        return Step(
            messenger,
            abi.encodeCall(
                ITokenMessengerV2.depositForBurnWithHook,
                (
                    0,
                    domain,
                    bytes32(uint256(uint160(mintRecipient))),
                    usdc,
                    bytes32(uint256(uint160(caller))),
                    maxFee,
                    finality,
                    envelope
                )
            ),
            usdc,
            min,
            ws
        );
    }

    function message(
        uint32 srcDomain,
        uint32 dstDomain,
        bytes32 nonce,
        address destinationCaller,
        address mintRecipient,
        address burnToken,
        uint256 amount,
        uint256 feeExecuted,
        bytes memory hook
    ) internal pure returns (bytes memory) {
        bytes32 messenger = bytes32(uint256(uint160(TOKEN_MESSENGER)));
        bytes memory header = abi.encodePacked(
            uint32(1),
            srcDomain,
            dstDomain,
            nonce,
            messenger,
            messenger,
            bytes32(uint256(uint160(destinationCaller))),
            uint32(1000),
            uint32(1000)
        );
        bytes memory body = abi.encodePacked(
            uint32(1),
            bytes32(uint256(uint160(burnToken))),
            bytes32(uint256(uint160(mintRecipient))),
            amount,
            bytes32(uint256(0x5e4de4)),
            feeExecuted,
            feeExecuted,
            uint256(0),
            hook
        );
        return bytes.concat(header, body);
    }
}
