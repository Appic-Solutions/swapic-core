// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import "../../src/interfaces/ISignatureTransfer.sol";

/// Etched at Permit2's address: Permit2's own `permitWitnessTransferFrom`
/// hashing (domain "Permit2", the PermitWitnessTransferFrom stub completed
/// by the witness type string), signer check, unordered nonce and deadline.
contract Permit2SigMock {
    bytes32 constant TOKEN_PERMISSIONS_TYPEHASH = keccak256("TokenPermissions(address token,uint256 amount)");
    string constant STUB =
        "PermitWitnessTransferFrom(TokenPermissions permitted,address spender,uint256 nonce,uint256 deadline,";

    mapping(address => mapping(uint256 => bool)) public usedNonce;

    function DOMAIN_SEPARATOR() public view returns (bytes32) {
        return keccak256(
            abi.encode(
                keccak256("EIP712Domain(string name,uint256 chainId,address verifyingContract)"),
                keccak256("Permit2"),
                block.chainid,
                address(this)
            )
        );
    }

    function digest(
        ISignatureTransfer.PermitTransferFrom memory permit,
        address spender,
        bytes32 witness,
        string memory witnessTypeString
    ) public view returns (bytes32) {
        bytes32 typeHash = keccak256(abi.encodePacked(STUB, witnessTypeString));
        bytes32 permitted = keccak256(abi.encode(TOKEN_PERMISSIONS_TYPEHASH, permit.permitted));
        bytes32 structHash =
            keccak256(abi.encode(typeHash, permitted, spender, permit.nonce, permit.deadline, witness));
        return keccak256(abi.encodePacked("\x19\x01", DOMAIN_SEPARATOR(), structHash));
    }

    function permitWitnessTransferFrom(
        ISignatureTransfer.PermitTransferFrom calldata permit,
        ISignatureTransfer.SignatureTransferDetails calldata details,
        address owner,
        bytes32 witness,
        string calldata witnessTypeString,
        bytes calldata signature
    ) external {
        require(block.timestamp <= permit.deadline, "SignatureExpired");
        require(details.requestedAmount <= permit.permitted.amount, "InvalidAmount");
        bytes32 d = digest(permit, msg.sender, witness, witnessTypeString);
        require(signature.length == 65, "InvalidSignatureLength");
        address signer = ecrecover(d, uint8(signature[64]), bytes32(signature[0:32]), bytes32(signature[32:64]));
        require(signer != address(0) && signer == owner, "InvalidSigner");
        require(!usedNonce[owner][permit.nonce], "InvalidNonce");
        usedNonce[owner][permit.nonce] = true;
        IERC20(permit.permitted.token).transferFrom(owner, details.to, details.requestedAmount);
    }
}
