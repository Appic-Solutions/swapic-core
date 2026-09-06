// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

contract GasHogMock {
    uint256 public sink;

    function burn() external {
        for (uint256 i = 0; i < 1_000_000; i++) {
            sink = i;
        }
    }
}
