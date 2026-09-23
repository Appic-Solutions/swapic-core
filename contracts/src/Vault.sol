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
import "@openzeppelin/contracts/utils/math/SafeCast.sol";
import "./interfaces/IERC3009.sol";
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

    /// one calldata word group for (v, r, s): without it pullWithAuthorization is
    /// stack-too-deep at solc 0.8.24. The canister's encoder has to match it.
    struct Signature {
        uint8 v;
        bytes32 r;
        bytes32 s;
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
    error PermitNotAuthorized();
    error NotAPermitToken();
    error SendFailed();

    /// completes Permit2's PermitWitnessTransferFrom typehash stub
    string private constant WITNESS_TYPE =
        "QuoteWitness witness)QuoteWitness(bytes32 quoteHash)TokenPermissions(address token,uint256 amount)";

    /// keccak256("Permit(address owner,address spender,uint256 value,uint256 nonce,uint256 deadline)"),
    /// the EIP-2612 typehash, and the value every USDC deployment that exposes a
    /// `PERMIT_TYPEHASH()` getter returns
    bytes32 private constant PERMIT_TYPEHASH = 0x6e71edae12b1b97f4d1f60370fef10105fa2faae0126114a169c64845d6126c9;

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
        uint256 received = IERC20(token).balanceOf(address(this)) - before;
        emit Deposited(quoteHash, token, msg.sender, received);
    }

    function depositNative(bytes32 quoteHash) external payable nonReentrant whenNotPaused(PauseClass.Deposits) {
        _markQuote(quoteHash, msg.sender);
        emit Deposited(quoteHash, address(0), msg.sender, msg.value);
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
    ///   - no allowance is ever created, so the standing-allowance precondition
    ///     that makes a swallowed permit dangerous never exists;
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
        emit Deposited(quoteHash, token, owner, IERC20(token).balanceOf(address(this)) - before);
    }

    /// The second gasless door, and only for a token that has EIP-2612 and not
    /// EIP-3009: in Phase 0 that means the bridged USDC.e, nothing else. Which
    /// door a token takes is settled by the quoter and the canister's config.
    ///
    /// The try/catch stays, because a permit is a bearer authorization: anyone
    /// who sees it may submit it, doing so spends the owner's nonce, and our own
    /// `permit` call then reverts through no fault of the user. Being front-run
    /// that way is griefing, not theft, and refusing the pull would hand a
    /// griefer a permanent denial of the gasless door for the price of one cheap
    /// transaction. What the catch branch may not do is shrug. It proves the
    /// permit the token consumed was ours: the token's own `DOMAIN_SEPARATOR()`
    /// and `nonces(owner)` are read, the EIP-2612 digest is rebuilt for the nonce
    /// just spent against (owner, this vault, amount, deadline), and the signature
    /// must recover to `owner`. A forged or borrowed signature cannot pass, so the
    /// standing allowance that every legacy depositor holds is no longer enough to
    /// move their funds. EOA only: raw ecrecover cannot verify an ERC-1271 wallet,
    /// which uses the Permit2 door instead (the 3009 door above takes only the
    /// (v, r, s) overload, so it is EOA only too).
    ///
    /// What the success branch trusts: that a `permit` which returns has checked
    /// the signature, as every EIP-2612 token does. A token whose `permit`
    /// returns without checking anything (a permissive fallback, WETH9's shape)
    /// is not a 2612 token, and the canister must never configure one for this
    /// door: on such a token the standing-allowance hole is open again.
    ///
    /// THE RESIDUAL, WHICH IS OPEN AND ACCEPTED, NOT CLOSED. An EIP-2612 permit
    /// names (owner, spender, value, deadline) and never names a quote, unlike
    /// Permit2's witness or 3009's nonce. So a genuine permit signed for quote A
    /// can be spent under quote B for the same token, amount and open deadline by
    /// whoever holds the quoter role, which both registers quotes and starts
    /// pulls. Against an outsider the door is closed; against a compromised
    /// quoter it is not. The owner has accepted this rather than adding a second
    /// typed-data prompt, on four grounds: it reaches only tokens that have 2612
    /// and not 3009, which in Phase 0 is bridged USDC.e alone; the loss is bounded
    /// by max_swap; it is bounded again by the permit window (the deadline and the
    /// canister's permit_deadline); and both the canister and this vault can be
    /// paused. Do not read the checks below as closing it.
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
        // the vault enforces the deadline itself, before anything else: `permit`
        // would enforce it on the success branch only, and the catch branch must
        // never run for an authorization that has already expired
        if (block.timestamp > deadline) revert PermitExpired();
        _markQuote(quoteHash, owner);

        try IERC20Permit(token).permit(owner, address(this), amount, deadline, v, r, s) {
            // unspent: the token has just set the allowance to exactly `amount`
        } catch {
            // the one tolerated failure is that this very permit was already mined
            // by somebody else, which spends exactly one nonce. Prove that, or refuse.
            _requireOurPermitWasConsumed(token, owner, amount, deadline, v, r, s);
        }

        uint256 before = IERC20(token).balanceOf(address(this));
        IERC20(token).safeTransferFrom(owner, address(this), amount);
        emit Deposited(quoteHash, token, owner, IERC20(token).balanceOf(address(this)) - before);
    }

    /// The signature the canister holds is the one the token just consumed, so
    /// the allowance it left behind is this vault's to spend. Reverts otherwise,
    /// which is every forged signature, every signature made for another spender,
    /// amount or deadline, and every permit with another one mined after it.
    function _requireOurPermitWasConsumed(
        address token,
        address owner,
        uint256 amount,
        uint256 deadline,
        uint8 v,
        bytes32 r,
        bytes32 s
    ) private view {
        uint256 next;
        bytes32 domain;
        // both getters are mandatory under EIP-2612, so a token missing either is
        // not a 2612 token. Where the missing getter reverts, as on USDbC and
        // Binance-Peg USDC, the refusal is named here; a token or address that
        // answers with no data at all still fails closed, but through the ABI
        // decoder's own revert, which try/catch does not catch
        try IERC20Permit(token).nonces(owner) returns (uint256 n) {
            next = n;
        } catch {
            revert NotAPermitToken();
        }
        try IERC20Permit(token).DOMAIN_SEPARATOR() returns (bytes32 d) {
            domain = d;
        } catch {
            revert NotAPermitToken();
        }
        // nothing was ever consumed for this owner, so nothing can have been ours
        if (next == 0) revert PermitNotAuthorized();

        // the domain is read, never recomputed: Polygon's bridged USDC.e signs
        // under an EIP712Domain with a salt and no chainId, which no chainId-based
        // formula produces
        bytes32 structHash = keccak256(abi.encode(PERMIT_TYPEHASH, owner, address(this), amount, next - 1, deadline));
        bytes32 digest = MessageHashUtils.toTypedDataHash(domain, structHash);
        (address signer, ECDSA.RecoverError err,) = ECDSA.tryRecover(digest, v, r, s);
        if (err != ECDSA.RecoverError.NoError || signer != owner) revert PermitNotAuthorized();
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
