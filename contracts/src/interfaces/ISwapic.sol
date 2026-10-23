// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import {Order, GaslessAuth, Permit2Auth} from "../SwapicTypes.sol";

/// The door: the only contract users approve. It takes exactly the signed
/// `amountIn` from the payer, once per order, hands it to the Runner, and
/// makes no other call.
interface ISwapic {
    event OrderStarted(
        bytes32 indexed id, bytes32 indexed ref, address indexed payer, address tokenIn, uint256 amountIn
    );
    event Paused(address by);
    event Unpaused(address by);
    event GuardianSet(address guardian);
    event CanisterSet(address canister);
    event OwnershipTransferStarted(address indexed previousOwner, address indexed newOwner);
    event OwnershipTransferred(address indexed previousOwner, address indexed newOwner);

    error IsPaused();
    error Expired();
    error Used();
    error NotPayer();
    error NotCanister();
    error NotOwner();
    error NotGuardianOrOwner();
    error BadValue();
    error NotAccepted();
    error BadParams();
    error Reentrancy();
    error NotReceived();

    /// By the payer. `tokenIn == NATIVE`: `msg.value == amountIn`, wrapped
    /// into `wrappedNative()` and handed to the Runner as that token (the
    /// steps are built for it); else `msg.value == 0` and Swapic pulls
    /// `amountIn` from the payer.
    function run(Order calldata order) external payable returns (bytes32 id);

    /// By the payer, ERC-20 only: a Permit2 witness transfer (witness = id).
    function runWithPermit2(Order calldata order, Permit2Auth calldata auth) external returns (bytes32 id);

    /// By the canister, ERC-20 only, through one of three gasless doors, each
    /// bound to `id`: the 3009 nonce, the 2612 QuoteAcceptance, the Permit2
    /// witness.
    function runFor(Order calldata order, GaslessAuth calldata auth) external returns (bytes32 id);

    function orderId(Order calldata order) external view returns (bytes32);
    /// What the payer signs on the 2612 door.
    function acceptanceDigest(Order calldata order) external view returns (bytes32);
    function used(bytes32 id) external view returns (bool);
    function runner() external view returns (address);
    function cctpReceiver() external view returns (address);
    function wrappedNative() external view returns (address);

    function paused() external view returns (bool);
    /// true while a door runs: the Runner refuses `fill` meanwhile, so no
    /// callback during a pull can act on the payer's tokens in flight
    function locked() external view returns (bool);
    function owner() external view returns (address);
    function pendingOwner() external view returns (address);
    function guardian() external view returns (address);
    /// the canister's EVM address
    function canister() external view returns (address);

    /// EIP-5267: the domain of the order id and the QuoteAcceptance.
    function eip712Domain()
        external
        view
        returns (
            bytes1 fields,
            string memory name,
            string memory version,
            uint256 chainId,
            address verifyingContract,
            bytes32 salt,
            uint256[] memory extensions
        );

    function pause() external;
    function unpause() external;
    function setGuardian(address guardian) external;
    function setCanister(address canister) external;
    function transferOwnership(address newOwner) external;
    function acceptOwnership() external;
}
