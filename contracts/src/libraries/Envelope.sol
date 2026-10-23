// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import {Action} from "../SwapicTypes.sol";

/// The sealed arrival instruction: `abi.encode` of 10 static words, 320
/// bytes, word i at byte 32 * i:
///
///   0  uint8   version      3
///   1  bytes32 ref          Order.ref
///   2  uint8   action       0 Deliver, 1 Swap, 2 CkDeposit
///   3  uint64  deadline     end of the canister-or-recipient window (CCTP)
///   4  address recipient    tokenOut receiver and refund receiver
///   5  address tokenOut     Swap only (NATIVE for ether)
///   6  uint256 minOut       Swap only, in tokenOut
///   7  bytes32 principal    CkDeposit only
///   8  bytes32 subaccount   CkDeposit only
///   9  uint64  minGas       gas `fill` must have left for the action
///
/// In a CCTP v2 message it starts at byte 376; in an Eco route it is the
/// `envelope` argument of `Runner.fill`.
library Envelope {
    uint256 internal constant VERSION = 3;
    uint256 internal constant LENGTH = 320;

    struct Data {
        uint8 version;
        bytes32 ref;
        Action action;
        uint64 deadline;
        address recipient;
        address tokenOut;
        uint256 minOut;
        bytes32 principal;
        bytes32 subaccount;
        uint64 minGas;
    }

    error BadEnvelope();

    function encode(Data memory d) internal pure returns (bytes memory) {
        return abi.encode(d);
    }

    /// Exactly 320 bytes, version 3, every narrow word clean.
    function decode(bytes calldata b) internal pure returns (Data memory d) {
        if (b.length != LENGTH) revert BadEnvelope();
        (
            uint256 version,
            bytes32 ref,
            uint256 action,
            uint256 deadline,
            uint256 recipient,
            uint256 tokenOut,
            uint256 minOut,
            bytes32 principal,
            bytes32 subaccount,
            uint256 minGas
        ) = abi.decode(b, (uint256, bytes32, uint256, uint256, uint256, uint256, uint256, bytes32, bytes32, uint256));
        if (
            version != VERSION || action > uint256(Action.CkDeposit) || deadline > type(uint64).max
                || recipient > type(uint160).max || tokenOut > type(uint160).max || minGas > type(uint64).max
        ) revert BadEnvelope();
        d = Data({
            version: uint8(version),
            ref: ref,
            action: Action(action),
            deadline: uint64(deadline),
            recipient: address(uint160(recipient)),
            tokenOut: address(uint160(tokenOut)),
            minOut: minOut,
            principal: principal,
            subaccount: subaccount,
            minGas: uint64(minGas)
        });
    }
}
