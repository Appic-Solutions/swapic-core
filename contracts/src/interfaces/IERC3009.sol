// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

/// EIP-3009 transfer with authorization, the two members the vault uses.
///
/// `receiveWithAuthorization` is the relayer-safe half of the standard: the token
/// requires `to == msg.sender`, so an authorization made out to this vault can
/// only ever be submitted by this vault. The nonce is an arbitrary 32 byte value
/// in a used-or-not map rather than a counter, which is what lets the vault put
/// the quote hash there and get a quote binding the standard pays nothing extra
/// for.
interface IERC3009 {
    function receiveWithAuthorization(
        address from,
        address to,
        uint256 value,
        uint256 validAfter,
        uint256 validBefore,
        bytes32 nonce,
        uint8 v,
        bytes32 r,
        bytes32 s
    ) external;

    function authorizationState(address authorizer, bytes32 nonce) external view returns (bool);
}
