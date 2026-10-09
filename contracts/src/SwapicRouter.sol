// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import {SwapParams, GaslessAuth, Permit2Auth, Rail, Action} from "./SwapicTypes.sol";
import {ISwapicRouter} from "./interfaces/ISwapicRouter.sol";
import {IEcoPortal} from "./interfaces/IEcoPortal.sol";
import {ITokenMessengerV2} from "./interfaces/ICctpV2.sol";
import {ISignatureTransfer} from "./interfaces/ISignatureTransfer.sol";
import {QuoteHash} from "./libraries/QuoteHash.sol";
import {HookData} from "./libraries/HookData.sol";
import {EcoIntent} from "./libraries/EcoIntent.sol";

/// The source-side entry point (spec 3.1 with the 4b fixes). Stateless but
/// for its configuration and `usedQuote`: every transaction ends with the
/// router holding exactly what it held before, and every approval it grants
/// is exact and reset to zero in the same call.
///
/// The money law, in the order the code applies it:
///   - the input is pulled only from msg.sender, or from the payer with an
///     authorization that binds the quote hash recomputed here from the very
///     params the router then acts on (the gasless doors, swapFor);
///   - the quote hash is spent before any pull, so a quote runs at most once;
///   - the DEX leg goes only to an allowlisted stateless router, never to a
///     token, a rail or the router, and its output is measured;
///   - the rail is reached only through the fixed adapters below, with the
///     recipient, the refund party and the amounts taken from the params and
///     the owner's registry, never from caller-supplied bytes.
contract SwapicRouter is ISwapicRouter {
    using SafeERC20 for IERC20;

    uint256 public constant MAX_FEE_BPS = 100;
    /// the fill window closes at least this long before the refund opens
    uint64 public constant ECO_REWARD_GAP = 300;

    ISignatureTransfer internal constant PERMIT2 = ISignatureTransfer(0x000000000022D473030F116dDEE9F6B43aC78BA3);
    /// completes Permit2's PermitWitnessTransferFrom typehash stub
    string internal constant WITNESS_TYPE =
        "QuoteWitness witness)QuoteWitness(bytes32 quoteHash)TokenPermissions(address token,uint256 amount)";
    bytes32 internal constant WITNESS_TYPEHASH = keccak256("QuoteWitness(bytes32 quoteHash)");

    address public immutable stable;
    address public immutable tokenMessenger;
    address public immutable ecoPortal;

    address public owner;
    address public pendingOwner;
    address public guardian;
    address public relayer;
    bool public paused;

    uint256 public feeBps;
    address public feeRecipient;

    address public ecoProver;
    uint64 public ecoMaxLockup = 2 hours;

    struct CctpDestination {
        uint32 domain;
        bytes32 executor;
        bool enabled;
    }

    mapping(address => bool) public allowedTarget;
    mapping(bytes32 => bool) public usedQuote;
    mapping(uint64 => CctpDestination) public cctpDestination;
    mapping(uint64 => EcoIntent.Destination) public ecoDestination;

    uint256 private _lock = 1;

    modifier nonReentrant() {
        if (_lock != 1) revert CallFailed();
        _lock = 2;
        _;
        _lock = 1;
    }

    modifier onlyOwner() {
        if (msg.sender != owner) revert NotOwner();
        _;
    }

    constructor(
        address stable_,
        address tokenMessenger_,
        address ecoPortal_,
        address owner_,
        address guardian_,
        address relayer_
    ) {
        if (stable_ == address(0) || owner_ == address(0)) revert BadParams();
        stable = stable_;
        tokenMessenger = tokenMessenger_;
        ecoPortal = ecoPortal_;
        owner = owner_;
        guardian = guardian_;
        relayer = relayer_;
        emit OwnershipTransferred(address(0), owner_);
    }

    // ------------------------------------------------------------------
    // entry points
    // ------------------------------------------------------------------

    /// @inheritdoc ISwapicRouter
    function swap(SwapParams calldata p) external nonReentrant returns (bytes32 q) {
        if (p.payer != msg.sender) revert NotPayer();
        q = _open(p);
        (uint256 inBefore, uint256 stableBefore) = _baseline(p.tokenIn);
        IERC20(p.tokenIn).safeTransferFrom(msg.sender, address(this), p.amountIn);
        _run(p, q, 0, inBefore, stableBefore);
    }

    /// @inheritdoc ISwapicRouter
    function swapWithPermit2(SwapParams calldata p, Permit2Auth calldata a) external nonReentrant returns (bytes32 q) {
        if (p.payer != msg.sender) revert NotPayer();
        q = _open(p);
        (uint256 inBefore, uint256 stableBefore) = _baseline(p.tokenIn);
        _pullPermit2(p, q, msg.sender, a.nonce, a.deadline, a.signature);
        _run(p, q, 0, inBefore, stableBefore);
    }

    /// @inheritdoc ISwapicRouter
    function swapFor(SwapParams calldata, GaslessAuth calldata) external pure returns (bytes32) {
        revert NotRelayer();
    }

    // ------------------------------------------------------------------
    // views
    // ------------------------------------------------------------------

    /// @inheritdoc ISwapicRouter
    function quoteHash(SwapParams calldata p) external view returns (bytes32) {
        return QuoteHash.quoteHash(p, block.chainid, address(this));
    }

    /// @inheritdoc ISwapicRouter
    function acceptanceDigest(SwapParams calldata p) external view returns (bytes32) {
        return QuoteHash.acceptanceDigest(
            p, QuoteHash.quoteHash(p, block.chainid, address(this)), block.chainid, address(this)
        );
    }

    /// @inheritdoc ISwapicRouter
    function hookData(SwapParams calldata p) external view returns (bytes memory) {
        return _hookData(p, QuoteHash.quoteHash(p, block.chainid, address(this)));
    }

    /// @inheritdoc ISwapicRouter
    function ecoIntent(SwapParams calldata p)
        external
        view
        returns (uint64 destination, bytes memory route, IEcoPortal.Reward memory reward, bytes32 intentHash)
    {
        destination = p.dstChainId;
        (route, reward, intentHash) =
            EcoIntent.compose(p, ecoDestination[p.dstChainId], ecoProver, stable, _hookData(p, p.ref));
    }

    /// @inheritdoc ISwapicRouter
    function eip712Domain()
        external
        view
        returns (bytes1, string memory, string memory, uint256, address, bytes32, uint256[] memory)
    {
        return (
            hex"0f",
            QuoteHash.DOMAIN_NAME,
            QuoteHash.DOMAIN_VERSION,
            block.chainid,
            address(this),
            bytes32(0),
            new uint256[](0)
        );
    }

    // ------------------------------------------------------------------
    // the source side
    // ------------------------------------------------------------------

    /// Refusals that touch nothing, then the quote hash, spent before any pull.
    function _open(SwapParams calldata p) internal returns (bytes32 q) {
        if (paused) revert IsPaused();
        if (block.timestamp > p.deadline) revert QuoteExpired();
        if (p.feeBps != feeBps) revert FeeMismatch();
        q = QuoteHash.quoteHash(p, block.chainid, address(this));
        if (usedQuote[q]) revert QuoteUsed();
        usedQuote[q] = true;
    }

    function _baseline(address tokenIn) internal view returns (uint256 inBefore, uint256 stableBefore) {
        inBefore = IERC20(tokenIn).balanceOf(address(this));
        stableBefore = tokenIn == stable ? inBefore : IERC20(stable).balanceOf(address(this));
    }

    function _pullPermit2(
        SwapParams calldata p,
        bytes32 q,
        address owner_,
        uint256 nonce,
        uint256 deadline,
        bytes calldata signature
    ) internal {
        PERMIT2.permitWitnessTransferFrom(
            ISignatureTransfer.PermitTransferFrom({
                permitted: ISignatureTransfer.TokenPermissions({token: p.tokenIn, amount: p.amountIn}),
                nonce: nonce,
                deadline: deadline
            }),
            ISignatureTransfer.SignatureTransferDetails({to: address(this), requestedAmount: p.amountIn}),
            owner_,
            keccak256(abi.encode(WITNESS_TYPEHASH, q)),
            WITNESS_TYPE,
            signature
        );
    }

    struct Outcome {
        uint256 platformFee;
        uint256 gasFee;
        uint256 unspentIn;
        uint256 bridged;
        uint256 excess;
        bytes32 railRef;
    }

    /// Everything after the pull: the leg, the fees, the rail, the events, and
    /// the check that the router holds exactly what it held before.
    function _run(SwapParams calldata p, bytes32 q, uint256 gasFee, uint256 inBefore, uint256 stableBefore) internal {
        Outcome memory o;
        uint256 out;
        (out, o.unspentIn) = _sourceLeg(p, inBefore, stableBefore);
        if (out < p.legMinOut) revert LegShort();

        o.gasFee = gasFee;
        o.platformFee = out * p.feeBps / 10_000;
        if (o.platformFee + gasFee > out) revert NotEnough();
        uint256 net = out - o.platformFee - gasFee;
        if (o.platformFee > 0) IERC20(stable).safeTransfer(feeRecipient, o.platformFee);
        if (gasFee > 0) IERC20(stable).safeTransfer(msg.sender, gasFee);

        if (p.rail == Rail.Cctp) {
            o.bridged = _cctp(p, q, net);
        } else {
            (o.bridged, o.excess, o.railRef) = _eco(p, net);
        }

        if (IERC20(stable).balanceOf(address(this)) != stableBefore) revert HeldFunds();
        if (IERC20(p.tokenIn).balanceOf(address(this)) != inBefore) revert HeldFunds();
        _emitStarted(p, q, o);
    }

    function _emitStarted(SwapParams calldata p, bytes32 q, Outcome memory o) internal {
        emit SwapStarted(q, p.payer, p.rail, p.amountIn, o.bridged, p.dst.recipient, p.dstChainId, p.ref, o.railRef);
        emit SwapFees(q, o.platformFee, o.gasFee, o.excess, o.unspentIn);
    }

    /// The measured stable the source side produced, and the unspent input
    /// returned to the payer.
    function _sourceLeg(SwapParams calldata p, uint256 inBefore, uint256 stableBefore)
        internal
        returns (uint256 out, uint256 unspentIn)
    {
        uint256 received = IERC20(p.tokenIn).balanceOf(address(this)) - inBefore;
        if (received == 0) revert NothingReceived();
        address target = p.leg.target;
        if (target == address(0)) {
            if (p.tokenIn != stable) revert BadParams();
            return (received, 0);
        }
        if (p.tokenIn == stable) revert BadParams();
        _checkTarget(target, p.tokenIn);

        IERC20(p.tokenIn).forceApprove(target, received);
        _call(target, p.leg.data);
        IERC20(p.tokenIn).forceApprove(target, 0);

        out = IERC20(stable).balanceOf(address(this)) - stableBefore;
        unspentIn = IERC20(p.tokenIn).balanceOf(address(this)) - inBefore;
        if (unspentIn > 0) IERC20(p.tokenIn).safeTransfer(p.payer, unspentIn);
    }

    /// CCTP v2: burns everything the source side produced. Deliver mints to
    /// the user with no destination caller; Swap and CkDeposit mint to the
    /// registry's executor and make it the only destination caller.
    function _cctp(SwapParams calldata p, bytes32 q, uint256 net) internal returns (uint256) {
        CctpDestination memory d = cctpDestination[p.dstChainId];
        if (!d.enabled) revert RailUnavailable();
        if (p.maxFee >= net) revert NotEnough();

        bytes32 mintRecipient;
        bytes32 caller;
        if (p.dst.action == Action.Deliver) {
            if (p.dst.recipient == bytes32(0)) revert BadParams();
            if (net - p.maxFee < p.dst.minOut) revert BelowMinOut();
            mintRecipient = p.dst.recipient;
        } else {
            if (d.executor == bytes32(0)) revert RailUnavailable();
            _checkExecutorAction(p);
            if (p.dst.action == Action.CkDeposit && net - p.maxFee < p.dst.minOut) revert BelowMinOut();
            mintRecipient = d.executor;
            caller = d.executor;
        }

        bytes memory hook = _hookData(p, q);
        IERC20(stable).forceApprove(tokenMessenger, net);
        ITokenMessengerV2(tokenMessenger).depositForBurnWithHook(
            net, d.domain, mintRecipient, stable, caller, p.maxFee, p.minFinality, hook
        );
        IERC20(stable).forceApprove(tokenMessenger, 0);
        return net;
    }

    /// Eco: publishes the intent the router composes itself, bridging exactly
    /// the quoted amount and returning the excess stable to the payer.
    function _eco(SwapParams calldata p, uint256 net)
        internal
        returns (uint256 bridged, uint256 excess, bytes32 intentHash)
    {
        EcoIntent.Destination memory d = ecoDestination[p.dstChainId];
        if (d.portal == address(0) || ecoProver == address(0) || ecoPortal == address(0)) revert RailUnavailable();
        _checkEvmRecipient(p.dst.recipient);
        if (p.dst.action == Action.Deliver) {
            if (p.eco.amountOut < p.dst.minOut) revert BelowMinOut();
        } else {
            if (d.executor == address(0)) revert RailUnavailable();
            _checkExecutorAction(p);
            if (p.dst.action == Action.CkDeposit && p.eco.amountOut < p.dst.minOut) revert BelowMinOut();
        }
        if (p.eco.amountOut == 0) revert BadParams();
        _checkEcoDeadlines(p.eco.routeDeadline, p.eco.rewardDeadline);

        bridged = p.eco.amount;
        if (bridged == 0) revert BadParams();
        if (net < bridged) revert NotEnough();
        excess = net - bridged;
        if (excess > 0) IERC20(stable).safeTransfer(p.payer, excess);

        bytes memory route;
        IEcoPortal.Reward memory reward;
        (route, reward, intentHash) = EcoIntent.compose(p, d, ecoProver, stable, _hookData(p, p.ref));

        uint256 before = IERC20(stable).balanceOf(address(this));
        IERC20(stable).forceApprove(ecoPortal, bridged);
        (bytes32 published,) = IEcoPortal(ecoPortal).publishAndFund(p.dstChainId, route, reward, false);
        IERC20(stable).forceApprove(ecoPortal, 0);
        if (published != intentHash) revert BadParams();
        if (before - IERC20(stable).balanceOf(address(this)) != bridged) revert RailTookWrongAmount();
    }

    function _checkEcoDeadlines(uint64 routeDeadline, uint64 rewardDeadline) internal view {
        if (routeDeadline <= block.timestamp) revert BadEcoDeadlines();
        if (uint256(routeDeadline) + ECO_REWARD_GAP > rewardDeadline) revert BadEcoDeadlines();
        if (rewardDeadline > block.timestamp + ecoMaxLockup) revert BadEcoDeadlines();
    }

    /// Swap and CkDeposit: the executor pays an EVM recipient, before a
    /// destination deadline still ahead.
    function _checkExecutorAction(SwapParams calldata p) internal view {
        _checkEvmRecipient(p.dst.recipient);
        if (p.dst.deadline <= block.timestamp) revert BadParams();
        if (p.dst.action == Action.Swap && (p.dst.tokenOut == address(0) || p.dst.minOut == 0)) revert BadParams();
    }

    function _checkEvmRecipient(bytes32 recipient) internal pure {
        if (recipient == bytes32(0) || uint256(recipient) >> 160 != 0) revert BadParams();
    }

    function _hookData(SwapParams calldata p, bytes32 ref) internal pure returns (bytes memory) {
        return HookData.encode(
            HookData.Data({
                version: HookData.VERSION,
                ref: ref,
                action: p.dst.action,
                deadline: p.dst.deadline,
                recipient: address(uint160(uint256(p.dst.recipient))),
                tokenOut: p.dst.tokenOut,
                minOut: p.dst.minOut,
                principal: p.dst.principal,
                subaccount: p.dst.subaccount
            })
        );
    }

    /// An allowlisted stateless DEX router, and never anything that holds or
    /// moves value on its own: a token, the stable, a rail, Permit2 or us.
    function _checkTarget(address target, address tokenIn) internal view {
        if (!allowedTarget[target]) revert TargetNotAllowed();
        if (target == tokenIn || _isFixed(target)) revert BadTarget();
    }

    function _isFixed(address target) internal view returns (bool) {
        return target == address(this) || target == stable || target == tokenMessenger || target == ecoPortal
            || target == address(PERMIT2);
    }

    /// No value, no returndata copied (return-bomb safe), and a failure reverts.
    function _call(address target, bytes calldata data) internal {
        bool ok;
        assembly ("memory-safe") {
            let ptr := mload(0x40)
            calldatacopy(ptr, data.offset, data.length)
            ok := call(gas(), target, 0, ptr, data.length, 0, 0)
        }
        if (!ok) revert CallFailed();
    }

    // ------------------------------------------------------------------
    // administration
    // ------------------------------------------------------------------

    function pause() external {
        if (msg.sender != guardian && msg.sender != owner) revert NotGuardianOrOwner();
        paused = true;
        emit Paused(msg.sender);
    }

    function unpause() external onlyOwner {
        paused = false;
        emit Unpaused(msg.sender);
    }

    function setGuardian(address guardian_) external onlyOwner {
        guardian = guardian_;
        emit GuardianSet(guardian_);
    }

    function setRelayer(address relayer_) external onlyOwner {
        relayer = relayer_;
        emit RelayerSet(relayer_);
    }

    function transferOwnership(address newOwner) external onlyOwner {
        pendingOwner = newOwner;
        emit OwnershipTransferStarted(owner, newOwner);
    }

    function acceptOwnership() external {
        if (msg.sender != pendingOwner) revert NotOwner();
        emit OwnershipTransferred(owner, msg.sender);
        owner = msg.sender;
        pendingOwner = address(0);
    }

    /// The allowlist holds ONLY stateless DEX routers (4b.11). A token is
    /// refused by probing `totalSupply()`; the stable, the rails, Permit2 and
    /// the router itself are refused by address; a target needs code.
    function setAllowedTarget(address target, bool allowed) external onlyOwner {
        if (allowed) {
            if (target.code.length == 0 || _isFixed(target)) revert BadTarget();
            (bool ok, bytes memory ret) = target.staticcall(abi.encodeWithSignature("totalSupply()"));
            if (ok && ret.length >= 32) revert BadTarget();
        }
        allowedTarget[target] = allowed;
        emit TargetAllowed(target, allowed);
    }

    function setFee(uint256 feeBps_, address feeRecipient_) external onlyOwner {
        if (feeBps_ > MAX_FEE_BPS || (feeBps_ > 0 && feeRecipient_ == address(0))) revert BadParams();
        feeBps = feeBps_;
        feeRecipient = feeRecipient_;
        emit FeeSet(feeBps_, feeRecipient_);
    }

    function setCctpDestination(uint64 chainId, uint32 domain, bytes32 executor, bool enabled) external onlyOwner {
        cctpDestination[chainId] = CctpDestination(domain, executor, enabled);
        emit CctpDestinationSet(chainId, domain, executor, enabled);
    }

    function setEcoDestination(uint64 chainId, address portal, address stable_, address executor) external onlyOwner {
        ecoDestination[chainId] = EcoIntent.Destination(portal, stable_, executor);
        emit EcoDestinationSet(chainId, portal, stable_, executor);
    }

    function setEcoConfig(address prover, uint64 maxLockup) external onlyOwner {
        ecoProver = prover;
        ecoMaxLockup = maxLockup;
        emit EcoConfigSet(prover, maxLockup);
    }
}
