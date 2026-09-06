// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "@openzeppelin/contracts/token/ERC20/ERC20.sol";

contract FeeOnTransferToken is ERC20 {
    constructor() ERC20("Fee", "FEE") {
        _mint(msg.sender, 1e27);
    }

    function _update(address from, address to, uint256 value) internal override {
        uint256 fee = from == address(0) ? 0 : value / 100; // mints are fee-free
        super._update(from, to, value - fee);
        if (fee > 0) super._update(from, address(0xdead), fee);
    }
}
