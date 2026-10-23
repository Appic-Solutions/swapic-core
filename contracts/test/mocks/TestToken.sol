// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "@openzeppelin/contracts/token/ERC20/ERC20.sol";

contract TestToken is ERC20 {
    constructor() ERC20("Test", "TST") {
        _mint(msg.sender, 1e27);
    }

    function mint(address to, uint256 amount) external {
        _mint(to, amount);
    }
}

/// Like WETH9: a fallback that accepts any selector and does nothing.
contract FallbackToken is TestToken {
    fallback() external payable {}

    receive() external payable {}
}

/// WETH9 in miniature.
contract WethMock is ERC20 {
    constructor() ERC20("Wrapped Ether", "WETH") {}

    function deposit() public payable {
        _mint(msg.sender, msg.value);
    }

    function withdraw(uint256 amount) external {
        _burn(msg.sender, amount);
        (bool ok,) = msg.sender.call{value: amount}("");
        require(ok, "eth");
    }

    receive() external payable {
        deposit();
    }
}

interface ITransferHook {
    function onTransfer(address from, address to, uint256 amount) external;
}

/// ERC-777 style: after every balance change it calls a hook, which runs
/// arbitrary code in the middle of whoever moves the token.
contract HookedToken is TestToken {
    address public hook;

    function setHook(address hook_) external {
        hook = hook_;
    }

    function _update(address from, address to, uint256 value) internal override {
        super._update(from, to, value);
        if (hook != address(0) && from != address(0)) ITransferHook(hook).onTransfer(from, to, value);
    }
}

/// Keeps 1% of every transfer between holders.
contract FeeToken is TestToken {
    function _update(address from, address to, uint256 value) internal override {
        if (from == address(0) || to == address(0)) return super._update(from, to, value);
        uint256 fee = value / 100;
        super._update(from, address(0xdead), fee);
        super._update(from, to, value - fee);
    }
}
