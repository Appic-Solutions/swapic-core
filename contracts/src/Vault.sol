// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "@openzeppelin/contracts-upgradeable/proxy/utils/Initializable.sol";
import "@openzeppelin/contracts-upgradeable/proxy/utils/UUPSUpgradeable.sol";
import "@openzeppelin/contracts-upgradeable/utils/ReentrancyGuardUpgradeable.sol";
import "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import "@openzeppelin/contracts/token/ERC20/IERC20.sol";

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

    error OnlyCanister();
    error OnlyGuardianOrCanister();
    error IsPaused();
    error QuoteHashUsed();
    error TargetNotAllowed();
    error DeltaMissed();
    error CallFailed();

    address public canister;
    address public guardian;

    mapping(PauseClass => bool) private _paused;
    mapping(bytes32 => bool) public usedQuoteHash;
    mapping(address => bool) public allowedRouter;

    event GuardianSet(address guardian);
    event NativeReceived(address from, uint256 amount);
    event ClassPaused(uint8 class_);
    event ClassUnpaused(uint8 class_);
    event Deposited(bytes32 indexed quoteHash, address indexed token, address indexed from, uint256 amount);
    event AllowlistSet(address target, bool ok);
    event Executed(bytes32 indexed swapRef);

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
        canister = canister_;
        guardian = guardian_;
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

    function _markQuote(bytes32 quoteHash) internal {
        if (usedQuoteHash[quoteHash]) revert QuoteHashUsed();
        usedQuoteHash[quoteHash] = true;
    }

    function deposit(bytes32 quoteHash, address token, uint256 amount)
        external
        nonReentrant
        whenNotPaused(PauseClass.Deposits)
    {
        _markQuote(quoteHash);
        uint256 before = IERC20(token).balanceOf(address(this));
        IERC20(token).safeTransferFrom(msg.sender, address(this), amount);
        uint256 received = IERC20(token).balanceOf(address(this)) - before;
        emit Deposited(quoteHash, token, msg.sender, received);
    }

    function depositNative(bytes32 quoteHash) external payable nonReentrant whenNotPaused(PauseClass.Deposits) {
        _markQuote(quoteHash);
        emit Deposited(quoteHash, address(0), msg.sender, msg.value);
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
            if (!allowedRouter[c.target]) revert TargetNotAllowed();
            if (c.approveAmount > 0) IERC20(c.approveToken).forceApprove(c.target, c.approveAmount);
            (bool ok,) = c.target.call{value: c.value}(c.data);
            if (!ok) revert CallFailed();
            if (c.approveAmount > 0) IERC20(c.approveToken).forceApprove(c.target, 0);
        }
    }

    function _checkDeltas(Delta[] calldata deltas, uint256[] memory beforeBalances) internal view {
        for (uint256 i = 0; i < deltas.length; i++) {
            int256 change = int256(_balance(deltas[i].token)) - int256(beforeBalances[i]);
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

    receive() external payable {
        emit NativeReceived(msg.sender, msg.value);
    }
}
