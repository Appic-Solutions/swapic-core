// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "@openzeppelin/contracts/token/ERC20/ERC20.sol";

/// The phantom-permit class: a real ERC20 whose `permit` returns without
/// checking anything, because it has no `permit` at all and a permissive
/// fallback answers the call (WETH9's shape). A relayer that trusts a `permit`
/// that returns is trusting nothing here, and a standing allowance is all
/// that stands between the owner's funds and whoever holds a pull.
contract PhantomPermitToken is ERC20 {
    constructor() ERC20("Phantom", "PHNT") {
        _mint(msg.sender, 1e27);
    }

    fallback() external {}
}
