// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import "@openzeppelin/contracts/utils/cryptography/ECDSA.sol";

/// An ERC-1271 contract wallet whose one signer key speaks for it. An honest
/// wallet answers the magic value exactly when that key signed the hash; a
/// rejecting one never does, whoever signed. It holds its own tokens and
/// grants its own allowances, as a Safe does through a transaction.
contract ContractWalletMock {
    bytes4 internal constant MAGIC = 0x1626ba7e;

    address public immutable signer;
    bool public immutable honest;

    constructor(address signer_, bool honest_) {
        signer = signer_;
        honest = honest_;
    }

    function isValidSignature(bytes32 hash, bytes calldata signature) external view returns (bytes4) {
        (address recovered, ECDSA.RecoverError err,) = ECDSA.tryRecover(hash, signature);
        if (honest && err == ECDSA.RecoverError.NoError && recovered == signer) return MAGIC;
        return 0xffffffff;
    }

    function approve(IERC20 token, address spender, uint256 amount) external {
        require(msg.sender == signer, "wallet: not the signer");
        token.approve(spender, amount);
    }
}
