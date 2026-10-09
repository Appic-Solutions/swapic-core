// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import {SwapParams, GaslessAuth, Permit2Auth, Rail} from "../SwapicTypes.sol";
import {IEcoPortal} from "./IEcoPortal.sol";

/// The source-side entry point, one per chain. It holds nothing after a
/// transaction: it pulls the input, runs the optional DEX leg, takes the fees
/// in the stable, and hands the measured amount to the rail in the same call.
///
/// Every entry spends the quote hash once (`usedQuote`, set before any pull),
/// requires `block.timestamp <= params.deadline`, and refuses while paused.
interface ISwapicRouter {
    /// One per executed quote. `bridgedAmount` is what the rail took: on CCTP
    /// everything the source side produced after fees, on Eco exactly
    /// `params.eco.amount`. `railRef` is the Eco intent hash on Eco and zero on
    /// CCTP (the CCTP hook data carries the quote hash itself). `ref` is
    /// `params.ref`, which keys the destination leg of an Eco route.
    event SwapStarted(
        bytes32 indexed quoteHash,
        address indexed payer,
        Rail rail,
        uint256 amountIn,
        uint256 bridgedAmount,
        bytes32 recipient,
        uint64 dstChainId,
        bytes32 ref,
        bytes32 railRef
    );

    /// What the source side kept or returned, beside SwapStarted: the
    /// platform fee and the gas reimbursement (both in the stable), the
    /// excess stable an Eco route returned to the payer, and any unspent
    /// input the DEX leg returned to the payer.
    event SwapFees(
        bytes32 indexed quoteHash, uint256 platformFee, uint256 gasFee, uint256 excessStable, uint256 unspentIn
    );

    event Paused(address by);
    event Unpaused(address by);
    event GuardianSet(address guardian);
    event RelayerSet(address relayer);
    event OwnershipTransferStarted(address indexed previousOwner, address indexed newOwner);
    event OwnershipTransferred(address indexed previousOwner, address indexed newOwner);
    event TargetAllowed(address indexed target, bool allowed);
    event FeeSet(uint256 feeBps, address feeRecipient);
    event CctpDestinationSet(uint64 indexed chainId, uint32 domain, bytes32 executor, bool enabled);
    event EcoDestinationSet(uint64 indexed chainId, address portal, address stable, address executor);
    event EcoConfigSet(address prover, uint64 maxLockup);

    error IsPaused();
    error NotOwner();
    error NotGuardianOrOwner();
    error NotRelayer();
    error NotPayer();
    error QuoteUsed();
    error QuoteExpired();
    error FeeMismatch();
    error GasFeeTooHigh();
    error TargetNotAllowed();
    error BadTarget();
    error BadParams();
    error LegShort();
    error NothingReceived();
    error NotEnough();
    error BelowMinOut();
    error RailUnavailable();
    error BadEcoDeadlines();
    error RailTookWrongAmount();
    error HeldFunds();
    error QuoteNotAccepted();
    error PermitExpired();
    error CallFailed();

    /// User-direct: pulls `amountIn` of `tokenIn` from msg.sender by
    /// transferFrom. Requires params.payer == msg.sender and maxGasFee unused.
    function swap(SwapParams calldata params) external returns (bytes32 quoteHash);

    /// User-direct through Permit2: a signature transfer whose owner is
    /// msg.sender, with the quote hash as witness (`QuoteWitness`, see abi.md).
    function swapWithPermit2(SwapParams calldata params, Permit2Auth calldata auth)
        external
        returns (bytes32 quoteHash);

    /// Gasless, callable only by the relayer (the canister's address). Pulls
    /// from params.payer through one of the three doors, then reimburses
    /// `auth.gasFee` (at most params.maxGasFee) to the relayer in the stable
    /// after the source leg.
    function swapFor(SwapParams calldata params, GaslessAuth calldata auth) external returns (bytes32 quoteHash);

    /// The quote hash of `params` for this router on this chain.
    function quoteHash(SwapParams calldata params) external view returns (bytes32);

    /// The EIP-712 digest of the QuoteAcceptance the payer signs for the 2612 door.
    function acceptanceDigest(SwapParams calldata params) external view returns (bytes32);

    /// The CCTP hook data this router attaches for `params` (ref = quote hash).
    function hookData(SwapParams calldata params) external view returns (bytes memory);

    /// The Eco intent this router publishes for `params`: the canonical route
    /// bytes to request Eco's quote with, the reward, and the intent hash.
    function ecoIntent(SwapParams calldata params)
        external
        view
        returns (uint64 destination, bytes memory route, IEcoPortal.Reward memory reward, bytes32 intentHash);

    function usedQuote(bytes32 quoteHash) external view returns (bool);
    function paused() external view returns (bool);
    function owner() external view returns (address);
    function guardian() external view returns (address);
    function relayer() external view returns (address);
    function feeBps() external view returns (uint256);
    function feeRecipient() external view returns (address);
    function allowedTarget(address target) external view returns (bool);
    /// the local stable (USDC): the rail token and the fee token
    function stable() external view returns (address);
    function tokenMessenger() external view returns (address);
    function ecoPortal() external view returns (address);
    function ecoProver() external view returns (address);
    function ecoMaxLockup() external view returns (uint64);
    function cctpDestination(uint64 chainId) external view returns (uint32 domain, bytes32 executor, bool enabled);
    function ecoDestination(uint64 chainId) external view returns (address portal, address stable, address executor);

    /// EIP-5267: the domain of the quote hash and the QuoteAcceptance.
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

    // ---- administration ----

    function pause() external;
    function unpause() external;
    function setGuardian(address guardian) external;
    function setRelayer(address relayer) external;
    function transferOwnership(address newOwner) external;
    function acceptOwnership() external;
    function setAllowedTarget(address target, bool allowed) external;
    function setFee(uint256 feeBps, address feeRecipient) external;
    function setCctpDestination(uint64 chainId, uint32 domain, bytes32 executor, bool enabled) external;
    function setEcoDestination(uint64 chainId, address portal, address stable, address executor) external;
    function setEcoConfig(address prover, uint64 maxLockup) external;
}
