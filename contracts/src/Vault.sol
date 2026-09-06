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

    error OnlyCanister();
    error OnlyGuardianOrCanister();
    error IsPaused();
    error QuoteHashUsed();

    address public canister;
    address public guardian;

    mapping(PauseClass => bool) private _paused;
    mapping(bytes32 => bool) public usedQuoteHash;

    event GuardianSet(address guardian);
    event NativeReceived(address from, uint256 amount);
    event ClassPaused(uint8 class_);
    event ClassUnpaused(uint8 class_);
    event Deposited(bytes32 indexed quoteHash, address indexed token, address indexed from, uint256 amount);

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

    receive() external payable {
        emit NativeReceived(msg.sender, msg.value);
    }
}
