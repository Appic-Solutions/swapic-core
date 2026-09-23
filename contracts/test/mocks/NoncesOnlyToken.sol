// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "@openzeppelin/contracts/token/ERC20/ERC20.sol";

/// Half of the EIP-2612 surface: `nonces()` answers, `DOMAIN_SEPARATOR()` and
/// `permit` do not exist. It is the only shape that reaches the second getter
/// check in the 2612 door's catch branch, because a token missing both getters
/// is refused at the first one. The nonce is settable so that a refusal cannot
/// come from the zero-nonce shortcut instead.
contract NoncesOnlyToken is ERC20 {
    mapping(address => uint256) public nonces;

    constructor() ERC20("Nonces Only", "NONCE") {
        _mint(msg.sender, 1e27);
    }

    function setNonce(address owner, uint256 nonce) external {
        nonces[owner] = nonce;
    }
}
