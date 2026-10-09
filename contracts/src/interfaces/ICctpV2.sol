// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

/// CCTP v2 TokenMessengerV2 (`0x28b5a0e9C621a5BadaA536219b3a228C8168cf5d` on
/// Base, Arbitrum and Ethereum), the one member the router calls.
interface ITokenMessengerV2 {
    /// selector 0x779b432d. Reverts on empty hook data, and on a `maxFee`
    /// below the messenger's own minimum fee for `amount`.
    function depositForBurnWithHook(
        uint256 amount,
        uint32 destinationDomain,
        bytes32 mintRecipient,
        address burnToken,
        bytes32 destinationCaller,
        uint256 maxFee,
        uint32 minFinalityThreshold,
        bytes calldata hookData
    ) external;
}

/// CCTP v2 MessageTransmitterV2 (`0x81D40F21F12A8F0E3252Bccb954D722d4c464B64`).
interface IMessageTransmitterV2 {
    /// selector 0x57ecfd28. Mints `amount - feeExecuted` to the message's mint
    /// recipient; only the destination caller may call it when that is set.
    function receiveMessage(bytes calldata message, bytes calldata attestation) external returns (bool);
}
