// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

/// CCTP v2 MessageTransmitterV2 (`0x81D40F21F12A8F0E3252Bccb954D722d4c464B64`).
interface IMessageTransmitterV2 {
    /// selector 0x57ecfd28. Mints `amount - feeExecuted` to the message's mint
    /// recipient; only the destination caller may call it when that is set.
    function receiveMessage(bytes calldata message, bytes calldata attestation) external returns (bool);
}
