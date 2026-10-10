// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import {DexCall, Action} from "./SwapicTypes.sol";
import {ISwapicExecutor} from "./interfaces/ISwapicExecutor.sol";
import {IMessageTransmitterV2} from "./interfaces/ICctpV2.sol";
import {HookData} from "./libraries/HookData.sol";

/// The ck minter's ERC-20 deposit helper on Ethereum: it pulls `amount` of
/// `erc20Address` from the caller for the minter and logs the beneficiary.
interface ICkDepositHelper {
    function depositErc20(address erc20Address, uint256 amount, bytes32 principal, bytes32 subaccount) external;
}

/// The destination-side executor (spec 3.2 with the 4b fixes). It holds
/// nothing between transactions and never reads a global balance as its own:
/// `completeCctp` and `release` act only on the delta the transmitter minted
/// to it inside the call, and `fill` only on the amount it pulled from its
/// caller inside the call (4b.6). Instructions come only from the attested
/// CCTP message or the Eco route, both fixed by the source transaction. The
/// only thing a caller supplies is CCTP swap data, and only a keeper or the
/// recipient may supply it, before the deadline (4b.3).
contract SwapicExecutor is ISwapicExecutor {
    using SafeERC20 for IERC20;

    /// CCTP v2 message layout (Circle evm-cctp-contracts at a92a2b4): the body at
    /// 148, the burn message's mint recipient at body + 36, its hook data at
    /// body + 228.
    uint256 internal constant MINT_RECIPIENT_AT = 148 + 36;
    uint256 internal constant HOOK_AT = 148 + 228;

    address public immutable stable;
    address public immutable transmitter;
    address public immutable ckHelper;

    address public owner;
    address public pendingOwner;
    address public guardian;
    bool public paused;

    mapping(address => bool) public isKeeper;
    mapping(address => bool) public allowedTarget;

    uint256 private _lock = 1;

    modifier nonReentrant() {
        if (_lock != 1) revert Reentrancy();
        _lock = 2;
        _;
        _lock = 1;
    }

    modifier whenNotPaused() {
        if (paused) revert IsPaused();
        _;
    }

    modifier onlyOwner() {
        if (msg.sender != owner) revert NotOwner();
        _;
    }

    modifier onlySelf() {
        if (msg.sender != address(this)) revert OnlySelf();
        _;
    }

    constructor(address stable_, address transmitter_, address ckHelper_, address owner_, address guardian_) {
        if (stable_ == address(0) || owner_ == address(0)) revert BadTarget();
        stable = stable_;
        transmitter = transmitter_;
        ckHelper = ckHelper_;
        owner = owner_;
        guardian = guardian_;
        emit OwnershipTransferred(address(0), owner_);
    }

    // ---- entry points ----

    /// @inheritdoc ISwapicExecutor
    function completeCctp(bytes calldata message, bytes calldata attestation, DexCall calldata swap)
        external
        nonReentrant
        whenNotPaused
    {
        HookData.Data memory h = _hookOf(message);
        if (h.action == Action.Deliver) revert WrongAction();
        if (block.timestamp > h.deadline) revert DeadlinePassed();
        if (!isKeeper[msg.sender] && msg.sender != h.recipient) revert NotKeeperOrRecipient();
        if (h.action == Action.CkDeposit && (swap.target != address(0) || swap.data.length != 0)) revert BadTarget();
        uint256 minted = _receive(message, attestation);
        _act(h, minted, swap);
    }

    /// Never paused: it is the exit that pays the user once the window closed.
    function release(bytes calldata message, bytes calldata attestation) external nonReentrant {
        HookData.Data memory h = _hookOf(message);
        if (block.timestamp <= h.deadline) revert DeadlineNotPassed();
        uint256 minted = _receive(message, attestation);
        IERC20(stable).safeTransfer(h.recipient, minted);
        emit Released(h.ref, h.recipient, minted);
    }

    /// @inheritdoc ISwapicExecutor
    function fill(uint256 amount, bytes calldata hookData, DexCall calldata swap) external nonReentrant whenNotPaused {
        HookData.Data memory h = HookData.decode(hookData);
        if (h.action == Action.Deliver) revert WrongAction();
        uint256 before = IERC20(stable).balanceOf(address(this));
        IERC20(stable).safeTransferFrom(msg.sender, address(this), amount);
        uint256 received = IERC20(stable).balanceOf(address(this)) - before;
        if (received == 0) revert NothingMinted();
        // the funds have arrived: from here on nothing reverts
        _act(h, received, swap);
    }

    // ---- internals ----

    /// The hook data of a CCTP message that mints to this executor.
    function _hookOf(bytes calldata message) internal view returns (HookData.Data memory) {
        if (message.length != HOOK_AT + HookData.LENGTH) revert BadMessage();
        if (bytes32(message[MINT_RECIPIENT_AT:MINT_RECIPIENT_AT + 32]) != bytes32(uint256(uint160(address(this))))) {
            revert BadMessage();
        }
        return HookData.decode(message[HOOK_AT:]);
    }

    /// Receives the message as its destination caller and returns exactly what
    /// the transmitter minted to this executor in this call.
    function _receive(bytes calldata message, bytes calldata attestation) internal returns (uint256 minted) {
        uint256 before = IERC20(stable).balanceOf(address(this));
        if (!IMessageTransmitterV2(transmitter).receiveMessage(message, attestation)) revert BadMessage();
        minted = IERC20(stable).balanceOf(address(this)) - before;
        if (minted == 0) revert NothingMinted();
    }

    /// Swap or CkDeposit on `amount`, else the stable to the recipient. The
    /// action runs in a self-call, so any failure in it unwinds it whole and
    /// only the fallback payment remains.
    function _act(HookData.Data memory h, uint256 amount, DexCall calldata swap) internal {
        if (h.action == Action.Swap) {
            try this.swapAndPay(amount, swap, h.tokenOut, h.minOut, h.recipient) returns (uint256 out, uint256 spent) {
                emit SwapCompleted(h.ref, h.recipient, h.tokenOut, spent, out);
                return;
            } catch {}
        } else if (h.action == Action.CkDeposit) {
            try this.ckDeposit(amount, h.principal, h.subaccount) {
                emit CkDeposited(h.ref, h.principal, h.subaccount, amount);
                return;
            } catch {}
        }
        IERC20(stable).safeTransfer(h.recipient, amount);
        emit SwapFellBack(h.ref, h.recipient, amount);
    }

    /// Self-call only. Exact approval of `amount` to an allowlisted router,
    /// reset to zero; at least `minOut` of tokenOut measured; all of it, and
    /// any stable the router did not spend, to the recipient.
    function swapAndPay(uint256 amount, DexCall calldata swap, address tokenOut, uint256 minOut, address recipient)
        external
        onlySelf
        returns (uint256 out, uint256 spent)
    {
        if (tokenOut == address(0) || tokenOut == stable) revert WrongAction();
        address target = swap.target;
        if (!allowedTarget[target]) revert TargetNotAllowed();
        if (target == tokenOut || _isFixed(target)) revert BadTarget();

        uint256 stableBefore = IERC20(stable).balanceOf(address(this));
        uint256 outBefore = IERC20(tokenOut).balanceOf(address(this));
        IERC20(stable).forceApprove(target, amount);
        _call(target, swap.data);
        IERC20(stable).forceApprove(target, 0);

        spent = stableBefore - IERC20(stable).balanceOf(address(this));
        if (spent > amount) revert SwapShort();
        out = IERC20(tokenOut).balanceOf(address(this)) - outBefore;
        if (out < minOut) revert SwapShort();
        IERC20(tokenOut).safeTransfer(recipient, out);
        if (amount > spent) IERC20(stable).safeTransfer(recipient, amount - spent);
    }

    /// Self-call only. The ck helper takes exactly `amount` for the principal
    /// and subaccount, through an exact approval reset to zero.
    function ckDeposit(uint256 amount, bytes32 principal, bytes32 subaccount) external onlySelf {
        if (ckHelper == address(0)) revert WrongAction();
        uint256 before = IERC20(stable).balanceOf(address(this));
        IERC20(stable).forceApprove(ckHelper, amount);
        ICkDepositHelper(ckHelper).depositErc20(stable, amount, principal, subaccount);
        IERC20(stable).forceApprove(ckHelper, 0);
        if (before - IERC20(stable).balanceOf(address(this)) != amount) revert CallFailed();
    }

    function _isFixed(address target) internal view returns (bool) {
        return target == address(this) || target == stable || target == transmitter || target == ckHelper;
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

    // ---- administration ----

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

    function setKeeper(address keeper, bool allowed) external onlyOwner {
        isKeeper[keeper] = allowed;
        emit KeeperSet(keeper, allowed);
    }

    /// Only stateless DEX routers (4b.11): never a token (probed by
    /// `totalSupply()`), the stable, the transmitter, the ck helper or itself.
    function setAllowedTarget(address target, bool allowed) external onlyOwner {
        if (allowed) {
            if (target.code.length == 0 || _isFixed(target)) revert BadTarget();
            (bool ok, bytes memory ret) = target.staticcall(abi.encodeWithSignature("totalSupply()"));
            if (ok && ret.length >= 32) revert BadTarget();
        }
        allowedTarget[target] = allowed;
        emit TargetAllowed(target, allowed);
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
