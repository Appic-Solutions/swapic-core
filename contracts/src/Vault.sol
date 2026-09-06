// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "@openzeppelin/contracts-upgradeable/proxy/utils/Initializable.sol";
import "@openzeppelin/contracts-upgradeable/proxy/utils/UUPSUpgradeable.sol";
import "@openzeppelin/contracts-upgradeable/utils/ReentrancyGuardUpgradeable.sol";
import "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import "@openzeppelin/contracts/token/ERC20/extensions/IERC20Permit.sol";
import "@openzeppelin/contracts/utils/math/SafeCast.sol";
import "./interfaces/ISignatureTransfer.sol";

contract Vault is Initializable, UUPSUpgradeable, ReentrancyGuardUpgradeable {
    using SafeERC20 for IERC20;

    enum PauseClass {
        Deposits,
        Executions,
        Payouts
    }

    struct Call {
        address target;
        uint256 value;
        bytes data;
        address approveToken;
        uint256 approveAmount;
    }

    struct Delta {
        address token;
        int256 minChange;
    }

    struct Item {
        bytes32 swapRef;
        Call[] calls;
        Delta[] deltas;
        uint256 gasLimit; // 0 = all
    }

    error OnlyCanister();
    error OnlyGuardianOrCanister();
    error IsPaused();
    error QuoteHashUsed();
    error TargetNotAllowed();
    error DeltaMissed();
    error CallFailed();
    error OnlySelf();
    error ZeroCanister();
    error PublicCallNotAllowed();
    error SendFailed();

    /// completes Permit2's PermitWitnessTransferFrom typehash stub
    string private constant WITNESS_TYPE =
        "QuoteWitness witness)QuoteWitness(bytes32 quoteHash)TokenPermissions(address token,uint256 amount)";

    ISignatureTransfer private constant PERMIT2 = ISignatureTransfer(0x000000000022D473030F116dDEE9F6B43aC78BA3);

    address public canister;
    address public guardian;

    mapping(PauseClass => bool) private _paused;
    /// keccak256(abi.encode(quoteHash, payer)) => spent. Scoped per payer so a
    /// stranger cannot burn someone else's quote hash as a griefing DoS.
    mapping(bytes32 => bool) public usedQuoteKey;
    mapping(address => bool) public allowedRouter;

    event GuardianSet(address guardian);
    event NativeReceived(address from, uint256 amount);
    event ClassPaused(uint8 class_);
    event ClassUnpaused(uint8 class_);
    event Deposited(bytes32 indexed quoteHash, address indexed token, address indexed from, uint256 amount);
    event AllowlistSet(address target, bool ok);
    event Executed(bytes32 indexed swapRef);
    event ItemResult(bytes32 indexed swapRef, bool ok);
    event Payout(bytes32 indexed ref, address token, address to, uint256 amount);
    event Refunded(bytes32 indexed ref, address token, address to, uint256 amount);
    /// distinct from Deposited on purpose: the atomic path settles in-tx, so it must
    /// never look like a cross-chain deposit the canister would credit a second time
    event AtomicSwap(bytes32 indexed quoteHash, address token, address from, uint256 amountIn);

    modifier onlyCanister() {
        if (msg.sender != canister) revert OnlyCanister();
        _;
    }

    modifier whenNotPaused(PauseClass class_) {
        if (_paused[class_]) revert IsPaused();
        _;
    }

    /// @custom:oz-upgrades-unsafe-allow constructor
    constructor() {
        _disableInitializers();
    }

    function initialize(address canister_, address guardian_) external initializer {
        __UUPSUpgradeable_init();
        __ReentrancyGuard_init();
        if (canister_ == address(0)) revert ZeroCanister();
        canister = canister_;
        guardian = guardian_; // zero is allowed: "no guardian yet"
    }

    function setGuardian(address guardian_) external onlyCanister {
        guardian = guardian_;
        emit GuardianSet(guardian_);
    }

    function paused(PauseClass class_) public view returns (bool) {
        return _paused[class_];
    }

    function pause(PauseClass class_) external {
        if (msg.sender != canister && msg.sender != guardian) revert OnlyGuardianOrCanister();
        _paused[class_] = true;
        emit ClassPaused(uint8(class_));
    }

    function unpause(PauseClass class_) external onlyCanister {
        _paused[class_] = false;
        emit ClassUnpaused(uint8(class_));
    }

    function _authorizeUpgrade(address) internal override onlyCanister {}

    function _quoteKey(bytes32 quoteHash, address payer) internal pure returns (bytes32) {
        return keccak256(abi.encode(quoteHash, payer));
    }

    function quoteKeyUsed(bytes32 quoteHash, address payer) external view returns (bool) {
        return usedQuoteKey[_quoteKey(quoteHash, payer)];
    }

    function _markQuote(bytes32 quoteHash, address payer) internal {
        bytes32 key = _quoteKey(quoteHash, payer);
        if (usedQuoteKey[key]) revert QuoteHashUsed();
        usedQuoteKey[key] = true;
    }

    function deposit(bytes32 quoteHash, address token, uint256 amount)
        external
        nonReentrant
        whenNotPaused(PauseClass.Deposits)
    {
        _markQuote(quoteHash, msg.sender);
        uint256 before = IERC20(token).balanceOf(address(this));
        IERC20(token).safeTransferFrom(msg.sender, address(this), amount);
        uint256 received = IERC20(token).balanceOf(address(this)) - before;
        emit Deposited(quoteHash, token, msg.sender, received);
    }

    function depositNative(bytes32 quoteHash) external payable nonReentrant whenNotPaused(PauseClass.Deposits) {
        _markQuote(quoteHash, msg.sender);
        emit Deposited(quoteHash, address(0), msg.sender, msg.value);
    }

    function pullWithPermit(
        bytes32 quoteHash,
        address token,
        address owner,
        uint256 amount,
        uint256 deadline,
        uint8 v,
        bytes32 r,
        bytes32 s
    ) external onlyCanister nonReentrant whenNotPaused(PauseClass.Deposits) {
        _markQuote(quoteHash, owner);
        // try/ignore: a front-run permit already set the allowance; transferFrom is the truth
        try IERC20Permit(token).permit(owner, address(this), amount, deadline, v, r, s) {} catch {}
        uint256 before = IERC20(token).balanceOf(address(this));
        IERC20(token).safeTransferFrom(owner, address(this), amount);
        emit Deposited(quoteHash, token, owner, IERC20(token).balanceOf(address(this)) - before);
    }

    function pullWithPermit2(
        bytes32 quoteHash,
        address owner,
        ISignatureTransfer.PermitTransferFrom calldata permit,
        bytes calldata signature
    ) external onlyCanister nonReentrant whenNotPaused(PauseClass.Deposits) {
        _markQuote(quoteHash, owner);
        address token = permit.permitted.token;
        uint256 before = IERC20(token).balanceOf(address(this));
        PERMIT2.permitWitnessTransferFrom(
            permit,
            ISignatureTransfer.SignatureTransferDetails(address(this), permit.permitted.amount),
            owner,
            keccak256(abi.encode(keccak256("QuoteWitness(bytes32 quoteHash)"), quoteHash)),
            WITNESS_TYPE,
            signature
        );
        emit Deposited(quoteHash, token, owner, IERC20(token).balanceOf(address(this)) - before);
    }

    function setRouterAllowlist(address target, bool ok) external onlyCanister {
        allowedRouter[target] = ok;
        emit AllowlistSet(target, ok);
    }

    function _balance(address token) internal view returns (uint256) {
        return token == address(0) ? address(this).balance : IERC20(token).balanceOf(address(this));
    }

    function _runCalls(Call[] calldata calls) internal {
        for (uint256 i = 0; i < calls.length; i++) {
            Call calldata c = calls[i];
            // never let a call re-enter the vault, even if it were allowlisted:
            // runItem is self-only but carries no reentrancy guard
            if (c.target == address(this)) revert TargetNotAllowed();
            if (!allowedRouter[c.target]) revert TargetNotAllowed();
            if (c.approveAmount > 0) IERC20(c.approveToken).forceApprove(c.target, c.approveAmount);
            bool ok;
            address target = c.target;
            uint256 value = c.value;
            bytes memory data = c.data;
            assembly {
                // no returndata copy: return-bomb safe
                ok := call(gas(), target, value, add(data, 0x20), mload(data), 0, 0)
            }
            if (!ok) revert CallFailed();
            if (c.approveAmount > 0) IERC20(c.approveToken).forceApprove(c.target, 0);
        }
    }

    function _checkDeltas(Delta[] calldata deltas, uint256[] memory beforeBalances) internal view {
        for (uint256 i = 0; i < deltas.length; i++) {
            int256 change = SafeCast.toInt256(_balance(deltas[i].token)) - SafeCast.toInt256(beforeBalances[i]);
            if (change < deltas[i].minChange) revert DeltaMissed();
        }
    }

    function execute(bytes32 swapRef, Call[] calldata calls, Delta[] calldata deltas)
        external
        onlyCanister
        nonReentrant
        whenNotPaused(PauseClass.Executions)
    {
        uint256[] memory beforeBalances = new uint256[](deltas.length);
        for (uint256 i = 0; i < deltas.length; i++) {
            beforeBalances[i] = _balance(deltas[i].token);
        }
        _runCalls(calls);
        _checkDeltas(deltas, beforeBalances);
        emit Executed(swapRef);
    }

    /// Atomic legacy same-chain path: the caller deposits and swaps in one tx.
    /// `payoutTo == address(0)` leaves the proceeds in the vault.
    function depositAndExecute(
        bytes32 quoteHash,
        address token,
        uint256 amount,
        Call[] calldata calls,
        address payoutToken,
        uint256 minOut,
        address payoutTo
    ) external nonReentrant whenNotPaused(PauseClass.Deposits) {
        // paying out is Payouts-gated too; keeping proceeds in the vault is not
        if (payoutTo != address(0) && _paused[PauseClass.Payouts]) revert IsPaused();
        _markQuote(quoteHash, msg.sender);

        uint256 beforeIn = IERC20(token).balanceOf(address(this));
        IERC20(token).safeTransferFrom(msg.sender, address(this), amount);
        uint256 received = IERC20(token).balanceOf(address(this)) - beforeIn;
        emit AtomicSwap(quoteHash, token, msg.sender, received);

        // anyone may call this, so the calls may only ever spend the caller's own
        // deposit: no native value, approvals only of `token`, capped at `received`
        uint256 totalApprove;
        for (uint256 i = 0; i < calls.length; i++) {
            if (calls[i].value != 0 || calls[i].approveToken != token) revert PublicCallNotAllowed();
            totalApprove += calls[i].approveAmount;
        }
        if (totalApprove > received) revert PublicCallNotAllowed();

        // snapshot after the pull so a token == payoutToken deposit never counts toward minOut
        uint256 beforeOut = _balance(payoutToken);
        _runCalls(calls);
        // signed: a payoutToken balance that fell reverts DeltaMissed, not an underflow panic
        int256 change = SafeCast.toInt256(_balance(payoutToken)) - SafeCast.toInt256(beforeOut);
        if (change < SafeCast.toInt256(minOut)) revert DeltaMissed();
        emit Executed(quoteHash);

        uint256 out = uint256(change);
        if (payoutTo != address(0) && out > 0) {
            IERC20(payoutToken).safeTransfer(payoutTo, out);
            emit Payout(quoteHash, payoutToken, payoutTo, out);
        }
    }

    function executeMany(Item[] calldata items)
        external
        onlyCanister
        nonReentrant
        whenNotPaused(PauseClass.Executions)
    {
        for (uint256 i = 0; i < items.length; i++) {
            uint256 gas_ = items[i].gasLimit == 0 ? gasleft() : items[i].gasLimit;
            bool ok;
            bytes memory callData = abi.encodeCall(this.runItem, (items[i]));
            address self = address(this);
            assembly {
                // no returndata copy: return-bomb safe
                ok := call(gas_, self, 0, add(callData, 0x20), mload(callData), 0, 0)
            }
            emit ItemResult(items[i].swapRef, ok);
        }
    }

    function runItem(Item calldata item) external {
        if (msg.sender != address(this)) revert OnlySelf();
        uint256[] memory beforeBalances = new uint256[](item.deltas.length);
        for (uint256 i = 0; i < item.deltas.length; i++) {
            beforeBalances[i] = _balance(item.deltas[i].token);
        }
        _runCalls(item.calls);
        _checkDeltas(item.deltas, beforeBalances);
    }

    function multicall(bytes[] calldata selfCalls) external onlyCanister {
        for (uint256 i = 0; i < selfCalls.length; i++) {
            (bool ok, bytes memory ret) = address(this).delegatecall(selfCalls[i]);
            if (!ok) {
                assembly {
                    revert(add(ret, 0x20), mload(ret))
                }
            }
        }
    }

    /// the single choke point for funds leaving the vault under canister control
    function _send(address token, address to, uint256 amount) internal {
        // todo_onchain_caps: per-token per-day limits land here when re-enabled
        if (token == address(0)) {
            (bool ok,) = to.call{value: amount}("");
            if (!ok) revert SendFailed();
        } else {
            IERC20(token).safeTransfer(to, amount);
        }
    }

    function payout(bytes32 swapRef, address token, address to, uint256 amount)
        external
        onlyCanister
        nonReentrant
        whenNotPaused(PauseClass.Payouts)
    {
        _send(token, to, amount);
        emit Payout(swapRef, token, to, amount);
    }

    function refund(bytes32 ref, address token, address to, uint256 amount)
        external
        onlyCanister
        nonReentrant
        whenNotPaused(PauseClass.Payouts)
    {
        _send(token, to, amount);
        emit Refunded(ref, token, to, amount);
    }

    receive() external payable {
        emit NativeReceived(msg.sender, msg.value);
    }
}
