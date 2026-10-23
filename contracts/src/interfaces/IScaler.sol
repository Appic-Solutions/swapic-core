// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

/// KyberSwap's InputScalingHelper (`0x2f577A41BeC1BE1152AeEA12e73b7391d15f655D`
/// on Base, Arbitrum and Ethereum): rewrites router calldata built for one
/// input amount to `newAmount`.
interface IScaler {
    function getScaledInputData(bytes calldata inputData, uint256 newAmount)
        external
        view
        returns (bool isSuccess, bytes memory data);
}
