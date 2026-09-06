// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "@openzeppelin/contracts/token/ERC20/extensions/ERC20Permit.sol";

contract PermitToken is ERC20Permit {
    constructor() ERC20("Permit", "PMT") ERC20Permit("Permit") {
        _mint(msg.sender, 1e27);
    }
}
