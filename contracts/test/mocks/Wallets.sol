// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import "@openzeppelin/contracts/utils/cryptography/ECDSA.sol";

/// A smart account whose key signs for it through ERC-1271.
contract Wallet1271 {
    address public immutable key;

    constructor(address key_) {
        key = key_;
    }

    function isValidSignature(bytes32 h, bytes calldata sig) external view returns (bytes4) {
        return ECDSA.recover(h, sig) == key ? bytes4(0x1626ba7e) : bytes4(0xffffffff);
    }

    function approve(address token, address spender) external {
        IERC20(token).approve(spender, type(uint256).max);
    }
}

/// A step or recipe target that calls back into our contracts and records
/// the selector of the refusal, then carries on, so the outer order still
/// succeeds and the record survives.
contract Reenterer {
    bytes4 public caught;
    address public target;
    bytes public data;

    function arm(address target_, bytes calldata data_) external {
        target = target_;
        data = data_;
    }

    function poke() public {
        (bool ok, bytes memory ret) = target.call(data);
        caught = ok ? bytes4(0xffffffff) : bytes4(ret);
    }

    /// a recipe: a full swap from its own float, then the callback
    function swapAndPoke(address tokenIn, address tokenOut, uint256 amountIn, uint256 amountOut) external {
        IERC20(tokenIn).transferFrom(msg.sender, address(this), amountIn);
        IERC20(tokenOut).transfer(msg.sender, amountOut);
        poke();
    }

    function approve(address token, address spender) external {
        IERC20(token).approve(spender, type(uint256).max);
    }
}
