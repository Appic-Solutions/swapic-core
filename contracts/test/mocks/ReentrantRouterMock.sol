// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "../../src/Vault.sol";

contract ReentrantRouterMock {
    Vault public immutable vault;

    constructor(Vault vault_) {
        vault = vault_;
    }

    // depositNative carries no canister gate, only the shared nonReentrant
    // guard held by the outer executeMany call: it is the vector that
    // actually exercises that guard (executeMany/execute would just hit
    // onlyCanister first, proving access control, not reentrancy).
    function attack() external {
        vault.depositNative{value: 0}("reentrant-deposit");
    }
}
