// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "@openzeppelin/contracts/token/ERC20/IERC20.sol";

contract EvilRouterMock {
    function swapAtoB(address a, address, uint256 amountIn, uint256) external {
        IERC20(a).transferFrom(msg.sender, address(this), amountIn);
    }
}
