// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

/// Returns a huge blob of returndata. A caller that copies returndata pays the
/// quadratic memory-expansion cost a second time in its own frame and runs out
/// of gas; a caller that ignores it (call(..., 0, 0)) is unharmed.
contract ReturnBombRouterMock {
    function bomb() external pure {
        assembly {
            return(0, 0x100000) // 1 MiB
        }
    }
}
