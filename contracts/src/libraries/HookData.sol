// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import {Action} from "../SwapicTypes.sol";

/// The typed, versioned instruction the source transaction fixes for the
/// destination: the CCTP burn's hook data, and the `hookData` argument of the
/// executor's `fill` in an Eco route. It is `abi.encode` of a static struct,
/// so it is always exactly 9 words (288 bytes), word i at byte offset 32 * i:
///
///   0  uint8   version      1
///   1  bytes32 ref          the quote hash on CCTP; SwapParams.ref on Eco
///   2  uint8   action       0 Deliver, 1 Swap, 2 CkDeposit
///   3  uint64  deadline     end of the keeper-or-user window
///   4  address recipient    the user: tokenOut receiver and fallback receiver
///   5  address tokenOut     Swap only, else zero
///   6  uint256 minOut       Swap: in tokenOut; CkDeposit: in the stable
///   7  bytes32 principal    CkDeposit only, else zero
///   8  bytes32 subaccount   CkDeposit only, else zero
///
/// In a CCTP v2 message the hook data starts at byte 376: the message body
/// starts at 148 and the burn message's hook data at 228 inside the body.
library HookData {
    uint8 internal constant VERSION = 1;
    uint256 internal constant LENGTH = 288;

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
    }

    error BadHookData();

    function encode(Data memory d) internal pure returns (bytes memory) {
        return abi.encode(d);
    }

    /// Refuses anything but exactly one version-1 instruction. `abi.decode`
    /// itself refuses an action out of range and dirty high bits in the
    /// narrow words.
    function decode(bytes memory b) internal pure returns (Data memory d) {
        if (b.length != LENGTH) revert BadHookData();
        d = abi.decode(b, (Data));
        if (d.version != VERSION) revert BadHookData();
    }
}
