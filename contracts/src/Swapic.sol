// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import {IERC20Permit} from "@openzeppelin/contracts/token/ERC20/extensions/IERC20Permit.sol";
import {ECDSA} from "@openzeppelin/contracts/utils/cryptography/ECDSA.sol";
import {SignatureChecker} from "@openzeppelin/contracts/utils/cryptography/SignatureChecker.sol";
import {NATIVE, PERMIT2, Order, GaslessAuth, Permit2Auth, Door} from "./SwapicTypes.sol";
import {ISwapic} from "./interfaces/ISwapic.sol";
import {IRunner} from "./interfaces/IRunner.sol";
import {IERC3009} from "./interfaces/IERC3009.sol";
import {IWrappedNative} from "./interfaces/IWrappedNative.sol";
import {ISignatureTransfer} from "./interfaces/ISignatureTransfer.sol";
import {OrderHash} from "./libraries/OrderHash.sol";
import {Runner} from "./Runner.sol";
import {CctpReceiver} from "./CctpReceiver.sol";

/// One per chain; its constructor deploys the CCTP receiver and the Runner.
/// It calls only: the token pull and balance reads, permit, Permit2, the
/// 3009 receive, the wrapped native token's deposit, and `runner.execute`.
contract Swapic is ISwapic {
    using SafeERC20 for IERC20;

    /// completes Permit2's PermitWitnessTransferFrom typehash stub
    string internal constant WITNESS_TYPE =
        "OrderWitness witness)OrderWitness(bytes32 orderId)TokenPermissions(address token,uint256 amount)";
    bytes32 internal constant WITNESS_TYPEHASH = keccak256("OrderWitness(bytes32 orderId)");

    address public immutable runner;
    address public immutable cctpReceiver;
    address public immutable wrappedNative;

    address public owner;
    address public pendingOwner;
    address public guardian;
    address public canister;
    bool public paused;

    mapping(bytes32 => bool) public used;

    uint256 private _lock = 1;

    modifier nonReentrant() {
        if (_lock != 1) revert Reentrancy();
        _lock = 2;
        _;
        _lock = 1;
    }

    modifier onlyOwner() {
        if (msg.sender != owner) revert NotOwner();
        _;
    }

    /// `tokenMessenger` is CCTP's TokenMessengerV2; `wrappedNative` the
    /// chain's WETH (WPOL on Polygon); `ckHelper` the ck minter's deposit
    /// helper on Ethereum, zero elsewhere.
    constructor(
        address usdc,
        address messageTransmitter,
        address tokenMessenger,
        address wrappedNative_,
        address ckHelper,
        address owner_,
        address guardian_,
        address canister_
    ) {
        if (owner_ == address(0) || canister_ == address(0)) revert BadParams();
        owner = owner_;
        guardian = guardian_;
        canister = canister_;
        wrappedNative = wrappedNative_;
        address receiver = address(new CctpReceiver(address(this), usdc, messageTransmitter, tokenMessenger));
        cctpReceiver = receiver;
        runner = address(new Runner(address(this), receiver, ckHelper, wrappedNative_));
        emit OwnershipTransferred(address(0), owner_);
        emit GuardianSet(guardian_);
        emit CanisterSet(canister_);
    }

    // ------------------------------------------------------------------
    // doors
    // ------------------------------------------------------------------

    function run(Order calldata o) external payable nonReentrant returns (bytes32 id) {
        if (o.payer != msg.sender) revert NotPayer();
        id = _open(o);
        uint256 inBaseline;
        if (o.tokenIn == NATIVE) {
            // wrapped here, so the steps never see ether they did not make
            if (msg.value != o.amountIn) revert BadValue();
            address w = wrappedNative;
            inBaseline = IERC20(w).balanceOf(runner);
            IWrappedNative(w).deposit{value: msg.value}();
            IERC20(w).safeTransfer(runner, o.amountIn);
            _arrived(w, runner, inBaseline, o.amountIn);
        } else {
            if (msg.value != 0) revert BadValue();
            inBaseline = IERC20(o.tokenIn).balanceOf(runner);
            IERC20(o.tokenIn).safeTransferFrom(msg.sender, runner, o.amountIn);
            _arrived(o.tokenIn, runner, inBaseline, o.amountIn);
        }
        emit OrderStarted(id, o.ref, o.payer, o.tokenIn, o.amountIn);
        IRunner(runner).execute(o, id, inBaseline);
    }

    function runWithPermit2(Order calldata o, Permit2Auth calldata a) external nonReentrant returns (bytes32 id) {
        if (o.payer != msg.sender) revert NotPayer();
        if (o.tokenIn == NATIVE) revert BadValue();
        id = _open(o);
        uint256 inBaseline = IERC20(o.tokenIn).balanceOf(runner);
        _pullPermit2(o, id, a.nonce, a.deadline, a.signature);
        _arrived(o.tokenIn, runner, inBaseline, o.amountIn);
        emit OrderStarted(id, o.ref, o.payer, o.tokenIn, o.amountIn);
        IRunner(runner).execute(o, id, inBaseline);
    }

    function runFor(Order calldata o, GaslessAuth calldata a) external nonReentrant returns (bytes32 id) {
        if (msg.sender != canister) revert NotCanister();
        if (o.tokenIn == NATIVE) revert BadValue();
        id = _open(o);
        IERC20 token = IERC20(o.tokenIn);
        uint256 inBaseline = token.balanceOf(runner);
        if (a.door == Door.Auth3009) {
            (uint8 v, bytes32 r, bytes32 s) = _split(a.signature);
            // a token whose fallback takes any selector makes this a silent
            // no-op: only the measured receipt counts
            uint256 own = token.balanceOf(address(this));
            IERC3009(o.tokenIn).receiveWithAuthorization(
                o.payer, address(this), o.amountIn, a.validAfter, a.validBefore, id, v, r, s
            );
            _arrived(o.tokenIn, address(this), own, o.amountIn);
            token.safeTransfer(runner, o.amountIn);
        } else if (a.door == Door.Permit2612) {
            // the permit carries no authority (a front-run of it sets the very
            // allowance needed); the payer's acceptance of `id` does
            if (!_accepted(o, id, a.acceptance)) revert NotAccepted();
            if (a.signature.length == 65) {
                (uint8 v, bytes32 r, bytes32 s) = _split(a.signature);
                try IERC20Permit(o.tokenIn).permit(o.payer, address(this), o.amountIn, o.deadline, v, r, s) {} catch {}
            }
            token.safeTransferFrom(o.payer, runner, o.amountIn);
        } else {
            _pullPermit2(o, id, a.nonce, a.deadline, a.signature);
        }
        _arrived(o.tokenIn, runner, inBaseline, o.amountIn);
        emit OrderStarted(id, o.ref, o.payer, o.tokenIn, o.amountIn);
        IRunner(runner).execute(o, id, inBaseline);
    }

    /// Not paused, in time, never seen: the id is spent before any pull.
    function _open(Order calldata o) internal returns (bytes32 id) {
        if (paused) revert IsPaused();
        if (block.timestamp > o.deadline) revert Expired();
        id = OrderHash.orderId(o, block.chainid, address(this));
        if (used[id]) revert Used();
        used[id] = true;
    }

    /// Exactly `amount` of `token` reached `holder` since `base`.
    function _arrived(address token, address holder, uint256 base, uint256 amount) internal view {
        if (IERC20(token).balanceOf(holder) != base + amount) revert NotReceived();
    }

    function _pullPermit2(Order calldata o, bytes32 id, uint256 nonce, uint256 deadline, bytes calldata signature)
        internal
    {
        ISignatureTransfer(PERMIT2).permitWitnessTransferFrom(
            ISignatureTransfer.PermitTransferFrom({
                permitted: ISignatureTransfer.TokenPermissions({token: o.tokenIn, amount: o.amountIn}),
                nonce: nonce,
                deadline: deadline
            }),
            ISignatureTransfer.SignatureTransferDetails({to: runner, requestedAmount: o.amountIn}),
            o.payer,
            keccak256(abi.encode(WITNESS_TYPEHASH, id)),
            WITNESS_TYPE,
            signature
        );
    }

    /// The payer's own key first, so an EIP-7702 account's key still speaks
    /// for it; only then, for a payer with code, its ERC-1271 answer.
    function _accepted(Order calldata o, bytes32 id, bytes calldata signature) internal view returns (bool) {
        bytes32 digest = OrderHash.acceptanceDigest(o, id, block.chainid, address(this));
        (address recovered, ECDSA.RecoverError err,) = ECDSA.tryRecover(digest, signature);
        if (err == ECDSA.RecoverError.NoError && recovered == o.payer) return true;
        return o.payer.code.length > 0 && SignatureChecker.isValidERC1271SignatureNow(o.payer, digest, signature);
    }

    function _split(bytes calldata sig) internal pure returns (uint8 v, bytes32 r, bytes32 s) {
        if (sig.length != 65) revert NotAccepted();
        r = bytes32(sig[0:32]);
        s = bytes32(sig[32:64]);
        v = uint8(sig[64]);
    }

    // ------------------------------------------------------------------
    // views
    // ------------------------------------------------------------------

    function orderId(Order calldata o) external view returns (bytes32) {
        return OrderHash.orderId(o, block.chainid, address(this));
    }

    function acceptanceDigest(Order calldata o) external view returns (bytes32) {
        return OrderHash.acceptanceDigest(
            o, OrderHash.orderId(o, block.chainid, address(this)), block.chainid, address(this)
        );
    }

    function locked() external view returns (bool) {
        return _lock != 1;
    }

    function eip712Domain()
        external
        view
        returns (bytes1, string memory, string memory, uint256, address, bytes32, uint256[] memory)
    {
        return (hex"0f", OrderHash.NAME, OrderHash.VERSION, block.chainid, address(this), bytes32(0), new uint256[](0));
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

    function setCanister(address canister_) external onlyOwner {
        if (canister_ == address(0)) revert BadParams();
        canister = canister_;
        emit CanisterSet(canister_);
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
}
