// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "@openzeppelin/contracts/token/ERC20/ERC20.sol";

/// An ordinary ERC20 anyone can deploy, with one extra: a one-shot arbitrary
/// call fired when a transfer leaves `hookFrom`. It stands for any token an
/// attacker controls (a deposit token, or a token on a pool path a router
/// walks), which is how attacker code gets a turn while the vault is waiting
/// on a call to return. Ported from the security review's proof of concept.
contract HookToken is ERC20 {
    address public hookFrom;
    address public hookTarget;
    bytes public hookData;
    bool public armed;

    constructor() ERC20("Hook", "HOOK") {
        _mint(msg.sender, 1e27);
    }

    function arm(address from, address target, bytes calldata data) external {
        hookFrom = from;
        hookTarget = target;
        hookData = data;
        armed = true;
    }

    function _update(address from, address to, uint256 value) internal override {
        super._update(from, to, value);
        if (armed && from == hookFrom) {
            armed = false;
            (bool ok,) = hookTarget.call(hookData);
            require(ok, "hook call failed");
        }
    }
}
