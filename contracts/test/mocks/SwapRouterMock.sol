// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "@openzeppelin/contracts/token/ERC20/IERC20.sol";

contract SwapRouterMock {
    function swapAtoB(address a, address b, uint256 amountIn, uint256 amountOut) external {
        IERC20(a).transferFrom(msg.sender, address(this), amountIn);
        IERC20(b).transfer(msg.sender, amountOut);
    }
}
