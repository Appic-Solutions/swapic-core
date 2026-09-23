// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import "../../src/interfaces/ISignatureTransfer.sol";

/// Etched at Permit2's address so a test can reach the Permit2 door without a
/// fork. It checks no signature. A paying one moves the requested amount from
/// the owner, as Permit2 does once the witness verifies; a hollow one accepts
/// the call and moves nothing. `pays` is immutable, so it survives the etch.
contract Permit2Mock {
    bool public immutable pays;

    constructor(bool pays_) {
        pays = pays_;
    }

    function permitWitnessTransferFrom(
        ISignatureTransfer.PermitTransferFrom calldata permit,
        ISignatureTransfer.SignatureTransferDetails calldata transferDetails,
        address owner,
        bytes32,
        string calldata,
        bytes calldata
    ) external {
        if (pays) {
            IERC20(permit.permitted.token).transferFrom(owner, transferDetails.to, transferDetails.requestedAmount);
        }
    }
}
