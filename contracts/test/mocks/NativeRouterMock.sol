// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "@openzeppelin/contracts/token/ERC20/IERC20.sol";

/// takes an ERC20 in, pays the caller back in native
contract NativeRouterMock {
    function swapForNative(address tokenIn, uint256 amountIn, uint256 amountOut) external {
        IERC20(tokenIn).transferFrom(msg.sender, address(this), amountIn);
        (bool ok,) = msg.sender.call{value: amountOut}("");
        require(ok, "native send failed");
    }

    receive() external payable {}
}
