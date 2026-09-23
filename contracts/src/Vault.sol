// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "@openzeppelin/contracts-upgradeable/proxy/utils/Initializable.sol";
import "@openzeppelin/contracts-upgradeable/proxy/utils/UUPSUpgradeable.sol";
import "@openzeppelin/contracts-upgradeable/utils/ReentrancyGuardUpgradeable.sol";
import "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import "@openzeppelin/contracts/token/ERC20/extensions/IERC20Permit.sol";
import "@openzeppelin/contracts/utils/cryptography/ECDSA.sol";
import "@openzeppelin/contracts/utils/cryptography/MessageHashUtils.sol";
import "@openzeppelin/contracts/utils/cryptography/SignatureChecker.sol";
import "@openzeppelin/contracts/interfaces/IERC5267.sol";
import "@openzeppelin/contracts/utils/math/SafeCast.sol";
import "./interfaces/IERC3009.sol";
import "./interfaces/ISignatureTransfer.sol";

contract Vault is Initializable, UUPSUpgradeable, ReentrancyGuardUpgradeable, IERC5267 {
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

    /// one calldata word group for (v, r, s): without it pullWithAuthorization is
    /// stack-too-deep at solc 0.8.24. The canister's encoder has to match it.
    struct Signature {
        uint8 v;
        bytes32 r;
        bytes32 s;
    }

    /// What the owner signs to authorize a relayed 2612 pull: EIP-712 typed
    /// data over this vault's own domain, field for field the type string in
    /// ACCEPTANCE_TYPEHASH. It carries the quote's readable economics so a
    /// wallet shows the deal and not an opaque hash. `dstToken` and
    /// `dstAddress` are the quote's own text forms, since a destination may be
    /// on Solana or the IC; the vault treats them as opaque signed bytes, and
    /// the canister checks every field against the quote.
    struct QuoteAcceptance {
        bytes32 quoteHash;
        address token;
        address owner;
        uint256 amount;
        uint256 deadline;
        uint256 dstChainId;
        string dstToken;
        string dstAddress;
        uint256 minOut;
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
    error PermitExpired();
    error QuoteNotAccepted();
    error NothingReceived();
    error SendFailed();

    /// completes Permit2's PermitWitnessTransferFrom typehash stub
    string private constant WITNESS_TYPE =
        "QuoteWitness witness)QuoteWitness(bytes32 quoteHash)TokenPermissions(address token,uint256 amount)";

    /// 0xae7d93fab40caaa623490511b488bb043eeeabacb34720e8ed73aad1299deeec. Never
    /// reuse it for another door unless the struct names that door.
    bytes32 private constant ACCEPTANCE_TYPEHASH = keccak256(
        "QuoteAcceptance(bytes32 quoteHash,address token,address owner,uint256 amount,uint256 deadline,uint256 dstChainId,string dstToken,string dstAddress,uint256 minOut)"
    );
    bytes32 private constant DOMAIN_TYPEHASH =
        keccak256("EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)");
    string private constant DOMAIN_NAME = "Swapic Vault";
    string private constant DOMAIN_VERSION = "1";

    ISignatureTransfer private constant PERMIT2 = ISignatureTransfer(0x000000000022D473030F116dDEE9F6B43aC78BA3);

    address public canister;
    address public guardian;

    mapping(PauseClass => bool) private _paused;
    /// keccak256(abi.encode(quoteHash, payer)) => spent. Scoped per payer so a
    /// stranger cannot burn someone else's quote hash as a griefing DoS.
    mapping(bytes32 => bool) public usedQuoteKey;
    /// What the canister's `execute` and `executeMany` may call, and nothing
    /// else reads it: see `setRouterAllowlist` for the law an entry must meet.
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
        _logDeposit(quoteHash, token, msg.sender, IERC20(token).balanceOf(address(this)) - before);
    }

    function depositNative(bytes32 quoteHash) external payable nonReentrant whenNotPaused(PauseClass.Deposits) {
        _markQuote(quoteHash, msg.sender);
        _logDeposit(quoteHash, address(0), msg.sender, msg.value);
    }

    /// Where every entry door ends, with the balance change it measured. A door
    /// that received nothing is refused, so a token that "succeeds" at moving
    /// nothing (a permissive fallback, WETH9's shape, or an amount of zero)
    /// cannot burn the payer's quote key or log a deposit of zero. Anything
    /// else is logged as measured, never as the amount that was asked for.
    function _logDeposit(bytes32 quoteHash, address token, address from, uint256 received) private {
        if (received == 0) revert NothingReceived();
        emit Deposited(quoteHash, token, from, received);
    }

    /// The first of the four gasless doors, and the one to reach for whenever the
    /// token has it: EIP-3009 `receiveWithAuthorization` with the quote hash as
    /// the authorization's nonce. Order of preference is 3009, then the 2612 door
    /// below (only where 3009 is absent), then Permit2, then legacy approve and
    /// deposit. Which door a token can take is a property of the token, settled by
    /// the quoter and the canister's config, never probed here.
    ///
    /// Nothing in this function checks a signature, and that is the point: the
    /// token does all of it, and does it better than a relayer can.
    ///   - no prior approval is needed, so it is gasless for a first-time user;
    ///   - no allowance is ever created, so the door leaves no standing
    ///     authorization behind for anything to spend later;
    ///   - the token requires `to == msg.sender`, so only this vault can submit
    ///     this authorization and there is no front-run to defend against;
    ///   - the nonce is an arbitrary 32 byte value in a used-or-not map, so
    ///     putting the quote hash there binds the authorization to one swap, and
    ///     the token keeps that replay guard independently of `usedQuoteKey`.
    ///
    /// Both of the token's time bounds are strict (`now > validAfter` and
    /// `now < validBefore` in Circle's implementation), so equality on either
    /// side reverts on chain: the window the canister signs has to allow for it.
    function pullWithAuthorization(
        bytes32 quoteHash,
        address token,
        address owner,
        uint256 amount,
        uint256 validAfter,
        uint256 validBefore,
        Signature calldata sig
    ) external onlyCanister nonReentrant whenNotPaused(PauseClass.Deposits) {
        _markQuote(quoteHash, owner);
        uint256 before = IERC20(token).balanceOf(address(this));
        IERC3009(token).receiveWithAuthorization(
            owner, address(this), amount, validAfter, validBefore, quoteHash, sig.v, sig.r, sig.s
        );
        _logDeposit(quoteHash, token, owner, IERC20(token).balanceOf(address(this)) - before);
    }

    /// The second gasless door, and only for a token that has EIP-2612 and not
    /// EIP-3009: in Phase 0 that means the bridged USDC.e, nothing else. Which
    /// door a token takes is settled by the quoter and the canister's config.
    ///
    /// The industry pattern for a relayed third-party permit (Across's
    /// SpokePoolPeriphery, 1inch's OrderMixin): the permit carries no authority
    /// at all. What authorizes the pull is the owner's QuoteAcceptance, EIP-712
    /// typed data over this vault's own domain, checked before the permit and
    /// before the pull. The permit is only how the allowance gets there without
    /// the user paying gas, so its outcome is swallowed with nothing in the
    /// catch: a griefer who mines it first has set the very allowance we need,
    /// a token with no 2612 simply leaves the allowance to be there already, and
    /// neither can authorize anything.
    ///
    /// The order is the contract: the deadline, then the quote key, then the
    /// acceptance, then the permit, then the pull, then `Deposited` with the
    /// measured delta. The acceptance is checked before anything is asked of
    /// the token, so a refused pull never touches it.
    ///
    /// Verification. ECDSA first: the owner's own key is accepted whether or
    /// not the owner has code, so an EIP-7702 account's key still speaks for it
    /// (OpenZeppelin 5.1's `isValidSignatureNow` would skip ECDSA for any owner
    /// with code). Only when that fails, and only for an owner with code, is
    /// ERC-1271 asked, so contract wallets are accepted as Permit2 and Across
    /// accept them. An ERC-1271 acceptance is exactly as strong as the wallet's
    /// own `isValidSignature`, which is the wallet's responsibility.
    ///
    /// The domain is (name "Swapic Vault", version "1", `block.chainid`, this
    /// vault's address), computed on every call and never cached: behind the
    /// proxy `address(this)` is the proxy, so an immutable cache built by the
    /// implementation's constructor would never hit, and a stored one would
    /// need a storage slot and a rebuild on a chain fork. `eip712Domain()`
    /// reports it (EIP-5267) so a wallet can build it without a frontend.
    ///
    /// BOTH 2612 RESIDUALS OF THE EARLIER DOOR ARE CLOSED, AND THIS IS WHY.
    ///   1. A compromised quoter can no longer move a permit to another quote.
    ///      The authority is the acceptance, and it names the quote hash, the
    ///      token, the owner, the amount, the deadline and the destination the
    ///      owner read, under this vault's domain on this chain. The quote key
    ///      spends it once. A genuine permit, even one this vault already
    ///      consumed, buys a replay nothing without a fresh acceptance of the
    ///      new quote, which only the owner can sign.
    ///   2. A phantom-permit token (one whose `permit` returns without checking
    ///      anything, WETH9's fallback shape) can no longer authorize anything.
    ///      A `permit` that returns is no longer read as a signature having
    ///      been checked; the vault checks the owner's signature itself, first.
    function pullWithPermit(
        QuoteAcceptance calldata acceptance,
        bytes calldata acceptanceSignature,
        Signature calldata permit
    ) external onlyCanister nonReentrant whenNotPaused(PauseClass.Deposits) {
        // the permit and the acceptance share the deadline, and the vault
        // enforces it itself before anything else
        if (block.timestamp > acceptance.deadline) revert PermitExpired();
        address owner = acceptance.owner;
        _markQuote(acceptance.quoteHash, owner);
        if (!_accepted(acceptance, acceptanceSignature)) revert QuoteNotAccepted();

        IERC20 token = IERC20(acceptance.token);
        uint256 amount = acceptance.amount;
        try IERC20Permit(address(token)).permit(
            owner, address(this), amount, acceptance.deadline, permit.v, permit.r, permit.s
        ) {} catch {}

        uint256 before = token.balanceOf(address(this));
        token.safeTransferFrom(owner, address(this), amount);
        _logDeposit(acceptance.quoteHash, address(token), owner, token.balanceOf(address(this)) - before);
    }

    /// Whether `signature` is the owner's acceptance of exactly `acceptance`,
    /// for this vault on this chain: the owner's own key first, then, for an
    /// owner with code, the owner's ERC-1271 answer.
    function _accepted(QuoteAcceptance calldata acceptance, bytes calldata signature) private view returns (bool) {
        bytes32 digest = MessageHashUtils.toTypedDataHash(_domainSeparator(), _hashAcceptance(acceptance));
        address owner = acceptance.owner;
        // tryRecover refuses a high `s`, a `v` other than 27 or 28 and a
        // recovery to the zero address, so an owner of zero never matches
        (address recovered, ECDSA.RecoverError err,) = ECDSA.tryRecover(digest, signature);
        if (err == ECDSA.RecoverError.NoError && recovered == owner) return true;
        return owner.code.length > 0 && SignatureChecker.isValidERC1271SignatureNow(owner, digest, signature);
    }

    function _hashAcceptance(QuoteAcceptance calldata a) private pure returns (bytes32) {
        return keccak256(
            abi.encode(
                ACCEPTANCE_TYPEHASH,
                a.quoteHash,
                a.token,
                a.owner,
                a.amount,
                a.deadline,
                a.dstChainId,
                keccak256(bytes(a.dstToken)),
                keccak256(bytes(a.dstAddress)),
                a.minOut
            )
        );
    }

    function _domainSeparator() private view returns (bytes32) {
        return keccak256(
            abi.encode(
                DOMAIN_TYPEHASH,
                keccak256(bytes(DOMAIN_NAME)),
                keccak256(bytes(DOMAIN_VERSION)),
                block.chainid,
                address(this)
            )
        );
    }

    /// EIP-5267: the acceptance's domain, as the vault computes it on this call.
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
        )
    {
        return (hex"0f", DOMAIN_NAME, DOMAIN_VERSION, block.chainid, address(this), bytes32(0), new uint256[](0));
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
        _logDeposit(quoteHash, token, owner, IERC20(token).balanceOf(address(this)) - before);
    }

    /// The one allowlist, and the law an entry must meet.
    ///
    /// The vault has no public execution door. A door that pays out what a
    /// pooled vault gained cannot be made safe: any hook token on any router's
    /// path gives the caller's code a turn, and that code can make a third
    /// party pay the vault (a permissionless escrow refund, a solver filling
    /// our own intent) while the door is measuring. So the only calls the vault
    /// ever issues are the canister's, through `execute` and `executeMany`
    /// (and `multicall` over them), and the law follows from that:
    ///   - every target is called only with calldata the canister chose. No
    ///     stranger can make the vault issue a call, so no stranger can speak
    ///     for the vault to anything on this list;
    ///   - so a target may hold balances the vault can claim. Eco's Portal
    ///     escrows a reward whose creator is the vault, and only the vault may
    ///     name where `refundTo` sends it: listing it is safe because only the
    ///     canister can make the vault call it;
    ///   - never a token contract. A listed token would let a call move the
    ///     vault's funds by the token's own `transfer`, around `_send`, the
    ///     single choke point `payout` and `refund` share.
    ///
    /// Carried to Plan 5 (security review L1). The canister's own swap leg can
    /// still hand foreign code a turn inside `execute`: a hook in the token, or
    /// in a token on a pool the route walks. That code can trigger the same
    /// third-party payments to the vault while the leg runs, and
    /// `Delta.minChange` is a vault-wide balance delta, so it would count them
    /// toward the leg's floor. A canister swap leg over a token the canister has
    /// not vetted must not trust the vault-wide balance delta while foreign code
    /// runs.
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
        // a native send to address(0) succeeds and burns the funds, so the door refuses it
        if (to == address(0)) revert SendFailed();
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
