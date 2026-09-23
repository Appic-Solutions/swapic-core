// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

/// An EIP-7702 delegate that exposes execution but no ERC-1271, like many
/// batching delegates. An EOA delegated to it has code, so a verifier that
/// asks an owner with code for ERC-1271 alone refuses the EOA's own key.
contract BatchOnlyDelegate {
    function execute(address to, bytes calldata data) external {
        (bool ok,) = to.call(data);
        require(ok, "delegate: call failed");
    }
}
