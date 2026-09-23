// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "forge-std/Test.sol";
import "../../src/Vault.sol";

/// Signs a QuoteAcceptance the way a wallet does, from the literal type string
/// and the literal domain values, never from anything the vault reports. A
/// vault that hashed anything else would refuse every signature made here.
abstract contract AcceptanceSigner is Test {
    /// keccak256 of the literal type string below, pinned by cast
    bytes32 internal constant ACCEPTANCE_TYPEHASH = 0xae7d93fab40caaa623490511b488bb043eeeabacb34720e8ed73aad1299deeec;
    string internal constant ACCEPTANCE_TYPE =
        "QuoteAcceptance(bytes32 quoteHash,address token,address owner,uint256 amount,uint256 deadline,uint256 dstChainId,string dstToken,string dstAddress,uint256 minOut)";

    /// a quote's own readable economics, as the canister stores them
    uint256 internal constant DST_CHAIN = 42161;
    string internal constant DST_TOKEN = "0xaf88d065e77c8cC2239327C5EDb3A432268e5831";
    string internal constant DST_ADDRESS = "0x7551A66653f9a20979ed81835a0b7008EC83401b";
    uint256 internal constant MIN_OUT = 24_900_000;

    function _domain(address verifyingContract, uint256 chainId) internal pure returns (bytes32) {
        return keccak256(
            abi.encode(
                keccak256("EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)"),
                keccak256("Swapic Vault"),
                keccak256("1"),
                chainId,
                verifyingContract
            )
        );
    }

    function _structHash(Vault.QuoteAcceptance memory a) internal pure returns (bytes32) {
        return keccak256(
            abi.encode(
                ACCEPTANCE_TYPEHASH,
                a.quoteHash,
                a.token,
                a.owner,
                a.amount,
                a.deadline,
                a.dstChainId,
                keccak256(bytes(a.dstToken)),
                keccak256(bytes(a.dstAddress)),
                a.minOut
            )
        );
    }

    function _digestFor(Vault.QuoteAcceptance memory a, address verifyingContract, uint256 chainId)
        internal
        pure
        returns (bytes32)
    {
        return keccak256(abi.encodePacked("\x19\x01", _domain(verifyingContract, chainId), _structHash(a)));
    }

    function _signFor(uint256 key, Vault.QuoteAcceptance memory a, address verifyingContract, uint256 chainId)
        internal
        pure
        returns (bytes memory)
    {
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(key, _digestFor(a, verifyingContract, chainId));
        return abi.encodePacked(r, s, v);
    }
}
