// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import {NATIVE, PERMIT2, Order, Step, Write, Payout, Recipe, Action} from "./SwapicTypes.sol";
import {IRunner} from "./interfaces/IRunner.sol";
import {ISwapic} from "./interfaces/ISwapic.sol";
import {IScaler} from "./interfaces/IScaler.sol";
import {ICkHelper} from "./interfaces/ICkHelper.sol";
import {Envelope} from "./libraries/Envelope.sol";

/// The hands. Every balance it checks is relative to the start of the call,
/// so a donation changes nothing; every approval it grants is exact and
/// reset after the call it was for.
contract Runner is IRunner {
    using SafeERC20 for IERC20;

    /// A self-call starved anywhere below it leaves `fill` about 1/64 of the
    /// gas for every frame between them (63/64 rule), so a starved arrival
    /// ends with at most 1/8 left for routers up to eight frames deep; the
    /// envelope's signed `minGas` covers deeper ones. An action that fails by
    /// burning all the gas it is given also reverts, at any gas limit.
    uint256 internal constant STARVED = 8;

    address public immutable swapic;
    address public immutable cctpReceiver;
    address public immutable ckHelper;
    /// an order with `tokenIn == NATIVE` arrives wrapped in this token
    address public immutable wrappedNative;

    uint256 private _lock = 1;

    modifier nonReentrant() {
        if (_lock != 1) revert Reentrancy();
        _lock = 2;
        _;
        _lock = 1;
    }

    constructor(address swapic_, address cctpReceiver_, address ckHelper_, address wrappedNative_) {
        swapic = swapic_;
        cctpReceiver = cctpReceiver_;
        ckHelper = ckHelper_;
        wrappedNative = wrappedNative_;
    }

    receive() external payable {}

    // ------------------------------------------------------------------
    // source: the signed steps
    // ------------------------------------------------------------------

    /// Swapic's only. The lock is the Runner's own: Swapic is not locked while
    /// a CCTP arrival runs `fill`, so without it a recipe could call `run` and
    /// nest a whole order inside the arrival.
    function execute(Order calldata o, bytes32 id, uint256 inBaseline) external nonReentrant {
        if (msg.sender != swapic) revert OnlySwapic();
        if (o.payout.token != address(0)) _checkRecipient(o.payout.recipient);
        (address[] memory tokens, uint256[] memory base) = _baselines(o, inBaseline);

        uint256 n = o.steps.length;
        for (uint256 i; i < n; ++i) {
            _step(o.steps[i], tokens, base);
        }

        Payout calldata p = o.payout;
        if (p.token != address(0)) {
            uint256 out = _delta(p.token, tokens, base);
            if (out < p.minOut) revert Short();
            _pay(p.token, p.recipient, out, p.minOut);
            emit PaidOut(id, p.recipient, p.token, out);
        }

        for (uint256 i; i < tokens.length; ++i) {
            if (_balance(tokens[i]) != base[i]) revert LeftOver();
        }
    }

    /// The tracked tokens (tokenIn, every step token, the payout token, each
    /// once) and their balances at the start of this order. A NATIVE tokenIn
    /// arrived wrapped, so the wrapped token is tracked in its place.
    function _baselines(Order calldata o, uint256 inBaseline)
        internal
        view
        returns (address[] memory tokens, uint256[] memory base)
    {
        uint256 n = o.steps.length;
        tokens = new address[](n + 2);
        base = new uint256[](n + 2);
        address tokenIn = o.tokenIn == NATIVE ? wrappedNative : o.tokenIn;
        uint256 count;
        for (uint256 i; i < n + 2; ++i) {
            address t = i == 0 ? tokenIn : i == n + 1 ? o.payout.token : o.steps[i - 1].token;
            if (t == address(0)) continue;
            bool seen;
            for (uint256 j; j < count; ++j) {
                if (tokens[j] == t) {
                    seen = true;
                    break;
                }
            }
            if (seen) continue;
            tokens[count] = t;
            if (t == tokenIn) base[count] = inBaseline;
            else if (t == NATIVE) base[count] = address(this).balance;
            else base[count] = IERC20(t).balanceOf(address(this));
            ++count;
        }
        assembly ("memory-safe") {
            mstore(tokens, count)
            mstore(base, count)
        }
    }

    function _step(Step calldata s, address[] memory tokens, uint256[] memory base) internal {
        address token = s.token;
        bytes memory data = s.data;
        uint256 d;
        if (token == address(0)) {
            if (s.writes.length != 0) revert BadWrite();
        } else {
            d = _delta(token, tokens, base);
            if (d < s.min) revert Short();
            _write(data, s.writes, d);
        }
        // after the writes: a write may cover the selector
        _guard(s.target, data);

        if (token == NATIVE) {
            _call(s.target, d, data);
        } else if (token != address(0)) {
            IERC20(token).forceApprove(s.target, d);
            _call(s.target, 0, data);
            IERC20(token).forceApprove(s.target, 0);
        } else {
            _call(s.target, 0, data);
        }
    }

    function _write(bytes memory data, Write[] calldata ws, uint256 d) internal pure {
        uint256 len = data.length;
        for (uint256 i; i < ws.length; ++i) {
            uint256 at = ws[i].at;
            uint256 minus = ws[i].minus;
            if (at + 32 > len || d < minus) revert BadWrite();
            uint256 v = d - minus;
            assembly ("memory-safe") {
                mstore(add(add(data, 32), at), v)
            }
        }
    }

    function _delta(address token, address[] memory tokens, uint256[] memory base) internal view returns (uint256) {
        for (uint256 i; i < tokens.length; ++i) {
            if (tokens[i] == token) return _balance(token) - base[i];
        }
        revert BadStep();
    }

    // ------------------------------------------------------------------
    // arrival
    // ------------------------------------------------------------------

    function fill(address token, uint256 amount, bytes calldata envelope, Recipe calldata recipe)
        external
        nonReentrant
    {
        ISwapic s = ISwapic(swapic);
        if (s.paused()) revert IsPaused();
        // a door is pulling tokens into this contract right now
        if (s.locked()) revert Reentrancy();
        Envelope.Data memory e = Envelope.decode(envelope);
        if (e.action == Action.Deliver) revert WrongAction();
        _checkRecipient(e.recipient);
        // else whoever picks the recipe could pay the arriving token to
        // themselves and still pass
        if (e.action == Action.Swap && (e.minOut == 0 || e.tokenOut == address(0) || e.tokenOut == token)) {
            revert BadArrival();
        }

        uint256 base = IERC20(token).balanceOf(address(this));
        IERC20(token).safeTransferFrom(msg.sender, address(this), amount);
        uint256 received = IERC20(token).balanceOf(address(this)) - base;

        if (_act(token, received, base, e, recipe)) return;
        IERC20(token).safeTransfer(e.recipient, received);
        emit Refunded(e.ref, e.recipient, token, received);
    }

    /// The action in an undoable self-call (no returndata copied). The signed
    /// `minGas` must be left for it; a self-call starved anyway is the
    /// caller's doing, not a failed action: both revert so the arrival can be
    /// retried with more gas.
    function _act(address token, uint256 received, uint256 base, Envelope.Data memory e, Recipe calldata recipe)
        internal
        returns (bool ok)
    {
        bytes memory cd = e.action == Action.Swap
            ? abi.encodeCall(this.swapAll, (token, received, base, e, recipe))
            : abi.encodeCall(this.ckDepositAll, (token, received, base, e));
        uint256 before = gasleft();
        // the self-call receives at most 63/64 of what is left here
        if (before - before / 64 < e.minGas) revert OutOfGas();
        assembly ("memory-safe") {
            ok := call(gas(), address(), 0, add(cd, 32), mload(cd), 0, 0)
        }
        if (!ok && gasleft() <= before / STARVED) revert OutOfGas();
    }

    /// All of `received` spent, at least `minOut` of tokenOut, all of it paid.
    function swapAll(address token, uint256 received, uint256 base, Envelope.Data calldata e, Recipe calldata r)
        external
        returns (uint256 out)
    {
        if (msg.sender != address(this)) revert OnlySelf();
        bytes memory data = r.data;
        if (r.scaler != address(0)) {
            bool ok;
            (ok, data) = IScaler(r.scaler).getScaledInputData(data, received);
            if (!ok) revert BadStep();
        }
        if (r.target == token) revert BadStep();
        _guard(r.target, data);

        address tokenOut = e.tokenOut;
        uint256 outBase = _balance(tokenOut);
        IERC20(token).forceApprove(r.target, received);
        _call(r.target, 0, data);
        IERC20(token).forceApprove(r.target, 0);
        if (IERC20(token).balanceOf(address(this)) != base) revert LeftOver();

        out = _balance(tokenOut) - outBase;
        if (out < e.minOut) revert Short();
        _pay(tokenOut, e.recipient, out, e.minOut);
        emit Delivered(e.ref, e.recipient, tokenOut, received, out);
    }

    function ckDepositAll(address token, uint256 received, uint256 base, Envelope.Data calldata e) external {
        if (msg.sender != address(this)) revert OnlySelf();
        if (ckHelper == address(0)) revert NoHelper();
        IERC20(token).forceApprove(ckHelper, received);
        ICkHelper(ckHelper).depositErc20(token, received, e.principal, e.subaccount);
        IERC20(token).forceApprove(ckHelper, 0);
        if (IERC20(token).balanceOf(address(this)) != base) revert LeftOver();
        emit CkDeposited(e.ref, e.principal, e.subaccount, received);
    }

    // ------------------------------------------------------------------
    // shared
    // ------------------------------------------------------------------

    /// Rule 2: never our own contracts or Permit2, never an address without
    /// code, never a call that moves or grants someone else's allowance.
    function _guard(address target, bytes memory data) internal view {
        if (
            target == swapic || target == address(this) || target == cctpReceiver || target == PERMIT2
                || target.code.length == 0
        ) revert BadStep();
        if (data.length >= 4) {
            bytes4 sel;
            assembly ("memory-safe") {
                sel := mload(add(data, 32))
            }
            if (_grants(sel)) revert BadStep();
        }
    }

    /// Calls that move someone else's tokens or leave a standing right with
    /// someone (the Runner makes its own exact approvals, so no step needs one).
    function _grants(bytes4 sel) internal pure returns (bool) {
        return sel == 0x23b872dd // transferFrom
            || sel == 0x095ea7b3 // approve
            || sel == 0x39509351 // increaseAllowance
            || sel == 0xd505accf // permit
            || sel == 0xa22cb465 // setApprovalForAll
            || sel == 0xd73dd623 // increaseApproval
            || sel == 0x959b8c3f // ERC-777 holder authorization
            || sel == 0x3177029f // ERC-1363 approveAndCall(address,uint256)
            || sel == 0xcae9ca51 // ERC-1363 approveAndCall(address,uint256,bytes)
            || sel == 0xdaea85c5 // DSToken approve(address)
            || sel == 0x558a7297 // ERC-6909 grant for all ids
            || sel == 0x426a8493 // ERC-6909 approve(address,uint256,uint256)
            || sel == 0x5df8122f // ERC-1820 setManager
            || sel == 0x29965a1d // ERC-1820 setInterfaceImplementer
            || sel == 0x110496e5 // Comet allow
            || sel == 0xeecea000 // Morpho setAuthorization
            || sel == 0xc04a8a10 // Aave approveDelegation
            || sel == 0xfa6e671d // Balancer setRelayerApproval
            || sel == 0xa3b22fc4; // Maker hope
    }

    /// Never nowhere, never one of our own contracts (funds there are stuck
    /// or takeable by anyone).
    function _checkRecipient(address to) internal view {
        if (to == address(0) || to == swapic || to == address(this) || to == cctpReceiver) revert BadRecipient();
    }

    function _balance(address token) internal view returns (uint256) {
        return token == NATIVE ? address(this).balance : IERC20(token).balanceOf(address(this));
    }

    /// All of `amount` to `to`, who must end up with at least `minOut` of an
    /// ERC-20 as received (a fee-on-transfer token pays less than it sends).
    function _pay(address token, address to, uint256 amount, uint256 minOut) internal {
        if (token == NATIVE) {
            _call(to, amount, "");
            return;
        }
        uint256 before = IERC20(token).balanceOf(to);
        IERC20(token).safeTransfer(to, amount);
        if (IERC20(token).balanceOf(to) - before < minOut) revert Short();
    }

    /// No returndata copied (return-bomb safe); a failure reverts.
    function _call(address target, uint256 value, bytes memory data) internal {
        bool ok;
        assembly ("memory-safe") {
            ok := call(gas(), target, value, add(data, 32), mload(data), 0, 0)
        }
        if (!ok) revert CallFailed();
    }
}
