// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import {DexCall} from "../SwapicTypes.sol";

/// The destination-side executor, one per chain. It holds nothing after a
/// transaction and acts only on the stable minted or pulled in the same call
/// (never on a global balance), on instructions that come only from the CCTP
/// message or the Eco route, both fixed by the source transaction.
///
/// `ref` in every event is the hook data's word 1: the quote hash on CCTP,
/// SwapParams.ref on Eco. An Eco `fill` is permissionless (the caller pays
/// with its own funds), so its events count only in a transaction that also
/// carries the Portal's `IntentFulfilled` for the swap's intent hash.
interface ISwapicExecutor {
    /// The swap paid the recipient `amountOut` of `tokenOut` (at least minOut)
    /// for `stableIn`; any unspent stable also went to the recipient.
    event SwapCompleted(
        bytes32 indexed ref, address indexed recipient, address tokenOut, uint256 stableIn, uint256 amountOut
    );
    /// The action could not complete (the swap failed or fell short, or the ck
    /// deposit failed): the recipient got `amount` of the stable instead.
    event SwapFellBack(bytes32 indexed ref, address indexed recipient, uint256 amount);
    /// The ck helper took `amount` of the stable for `principal` and `subaccount`.
    event CkDeposited(bytes32 indexed ref, bytes32 indexed principal, bytes32 subaccount, uint256 amount);
    /// After the deadline: the minted stable went to the recipient.
    event Released(bytes32 indexed ref, address indexed recipient, uint256 amount);

    event Paused(address by);
    event Unpaused(address by);
    event GuardianSet(address guardian);
    event KeeperSet(address indexed keeper, bool allowed);
    event TargetAllowed(address indexed target, bool allowed);
    event OwnershipTransferStarted(address indexed previousOwner, address indexed newOwner);
    event OwnershipTransferred(address indexed previousOwner, address indexed newOwner);

    error IsPaused();
    error NotOwner();
    error NotGuardianOrOwner();
    error NotKeeperOrRecipient();
    error DeadlinePassed();
    error DeadlineNotPassed();
    error BadMessage();
    error NothingMinted();
    error WrongAction();
    error TargetNotAllowed();
    error BadTarget();
    error OnlySelf();
    error SwapShort();
    error CallFailed();
    error Reentrancy();

    /// Before the hook data's deadline, by a keeper or the recipient only:
    /// receives the message as its destination caller (the USDC is minted to
    /// this executor in this call), then acts on exactly the minted amount.
    /// Swap: swaps through `swap` (an allowlisted router), paying the
    /// recipient at least minOut of tokenOut, else the stable. CkDeposit: the
    /// ck helper deposit (`swap` must be empty), else the stable to the
    /// recipient. A Deliver message never reaches the executor.
    function completeCctp(bytes calldata message, bytes calldata attestation, DexCall calldata swap) external;

    /// After the hook data's deadline, by anyone: receives the message and
    /// pays the whole minted stable to the recipient.
    function release(bytes calldata message, bytes calldata attestation) external;

    /// The Eco route's second call, after the route approved `amount` of the
    /// stable to this executor: pulls exactly `amount` from msg.sender, then
    /// acts as completeCctp does with the route's own `swap`. It never reverts
    /// after the pull, so a fill never fails because of the destination leg.
    function fill(uint256 amount, bytes calldata hookData, DexCall calldata swap) external;

    function paused() external view returns (bool);
    function owner() external view returns (address);
    function guardian() external view returns (address);
    /// the local USDC, what CCTP mints and what Eco routes deliver here
    function stable() external view returns (address);
    function transmitter() external view returns (address);
    /// the ck minter's ERC-20 deposit helper; zero except on Ethereum
    function ckHelper() external view returns (address);
    function isKeeper(address account) external view returns (bool);
    function allowedTarget(address target) external view returns (bool);

    // ---- administration ----

    function pause() external;
    function unpause() external;
    function setGuardian(address guardian) external;
    function setKeeper(address keeper, bool allowed) external;
    function setAllowedTarget(address target, bool allowed) external;
    function transferOwnership(address newOwner) external;
    function acceptOwnership() external;
}
