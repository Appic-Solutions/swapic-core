// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "@openzeppelin/contracts/token/ERC20/ERC20.sol";

/// DAI's permit, which is not EIP-2612: a different argument list (selector
/// 0x8fcbaf0c rather than 0xd505accf), a different typehash, a boolean instead
/// of a value, and an allowance of all or nothing. The vault's 2612 door must
/// fail closed on it twice over: no such function at the call, and the wrong
/// typehash when it rebuilds the digest.
contract DaiStylePermitToken is ERC20 {
    /// keccak256("Permit(address holder,address spender,uint256 nonce,uint256 expiry,bool allowed)")
    bytes32 public constant PERMIT_TYPEHASH = 0xea2aa0a1be11a07ed86d755c93467f4f82362b452371d1ba94d1715123511acb;

    // solhint-disable-next-line var-name-mixedcase
    bytes32 public immutable DOMAIN_SEPARATOR;
    mapping(address => uint256) public nonces;

    constructor() ERC20("Dai Stablecoin", "DAI") {
        _mint(msg.sender, 1e27);
        DOMAIN_SEPARATOR = keccak256(
            abi.encode(
                keccak256("EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)"),
                keccak256(bytes("Dai Stablecoin")),
                keccak256(bytes("1")),
                block.chainid,
                address(this)
            )
        );
    }

    function permit(
        address holder,
        address spender,
        uint256 nonce,
        uint256 expiry,
        bool allowed,
        uint8 v,
        bytes32 r,
        bytes32 s
    ) external {
        bytes32 digest = keccak256(
            abi.encodePacked(
                "\x19\x01",
                DOMAIN_SEPARATOR,
                keccak256(abi.encode(PERMIT_TYPEHASH, holder, spender, nonce, expiry, allowed))
            )
        );
        require(holder != address(0) && holder == ecrecover(digest, v, r, s), "Dai/invalid-permit");
        require(expiry == 0 || block.timestamp <= expiry, "Dai/permit-expired");
        require(nonce == nonces[holder]++, "Dai/invalid-nonce");
        _approve(holder, spender, allowed ? type(uint256).max : 0);
    }
}
