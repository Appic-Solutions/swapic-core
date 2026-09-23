// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

/// WETH9's shape without WETH9's deposit: balances, and a payable fallback
/// that accepts any call and does nothing. `transferFrom`,
/// `receiveWithAuthorization` and `permit` all "succeed" on it and move
/// nothing, so a door that trusts a call that returns would burn the payer's
/// quote key and log a deposit of zero.
contract FallbackToken {
    mapping(address => uint256) public balanceOf;

    fallback() external payable {}
}
