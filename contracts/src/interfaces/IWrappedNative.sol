// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

/// WETH9 and its kin (WPOL on Polygon).
interface IWrappedNative {
    function deposit() external payable;
}
