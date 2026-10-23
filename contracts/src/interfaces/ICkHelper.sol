// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

/// The ck minter's ERC-20 deposit helper on Ethereum: it pulls `amount` of
/// `erc20Address` from the caller for the minter and logs the beneficiary.
interface ICkHelper {
    function depositErc20(address erc20Address, uint256 amount, bytes32 principal, bytes32 subaccount) external;
}
