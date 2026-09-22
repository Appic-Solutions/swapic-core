// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "@openzeppelin/contracts/token/ERC20/IERC20.sol";

/// The smallest thing that stands in for every target that hands the vault money
/// without being asked: an escrow's permissionless `refund`, an intent settler's
/// `refundOnNonFill`, a CCTP `receiveMessage` whose mint recipient is the vault.
/// It takes no receiver argument at all, so the target itself is blameless: the
/// receiver is hardcoded to the vault, and it never pulls anything from the vault.
contract PaysTheVaultMock {
    IERC20 public immutable escrowed;
    address public immutable vault;

    constructor(IERC20 escrowed_, address vault_) {
        escrowed = escrowed_;
        vault = vault_;
    }

    function releaseToTheVault(uint256 amount) external {
        escrowed.transfer(vault, amount);
    }
}
