// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "forge-std/Test.sol";
import "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import "@openzeppelin/contracts/token/ERC20/extensions/IERC20Permit.sol";
import "../src/Vault.sol";
import "../src/interfaces/IERC3009.sol";
import "./helpers/AcceptanceSigner.sol";

/// The one 3009 read the vault never makes: the tests check the token's own
/// replay guard with it, so it lives here and not in the production interface.
interface IAuthorizationState {
    function authorizationState(address authorizer, bytes32 nonce) external view returns (bool);
}

/// Both gasless doors against the real Circle USDC on a Base fork. This is the
/// only place the signature work is proved end to end: the mocks agree with the
/// standard, and the standard is not what a user's wallet signs against, the
/// deployed token is. Skips itself without BASE_RPC_URL, like VaultPermit2.
contract VaultForkUSDCTest is AcceptanceSigner {
    address constant USDC = 0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913;
    uint256 constant FORK_BLOCK = 50960000;
    uint256 constant AMOUNT = 100e6;

    /// keccak256("ReceiveWithAuthorization(address from,address to,uint256 value,uint256 validAfter,uint256 validBefore,bytes32 nonce)")
    bytes32 constant RECEIVE_TYPEHASH = 0xd099cc98ef71107a616c4f0f941f04c322d8e254fe26b3c6668db87aae413de8;
    /// keccak256("Permit(address owner,address spender,uint256 value,uint256 nonce,uint256 deadline)")
    bytes32 constant PERMIT_TYPEHASH = 0x6e71edae12b1b97f4d1f60370fef10105fa2faae0126114a169c64845d6126c9;
    string forkUrl;
    Vault vault;
    address canister = address(0xCA);
    address guardian = address(0x6A);
    uint256 userKey = 0xA11CE;
    address user = vm.addr(userKey);
    uint256 strangerKey = 0xBADBEEF;
    address stranger = vm.addr(strangerKey);

    function setUp() public {
        forkUrl = vm.envOr("BASE_RPC_URL", string(""));
        if (bytes(forkUrl).length == 0) return; // CI has no RPC secret

        vm.createSelectFork(forkUrl, FORK_BLOCK);
        Vault impl = new Vault();
        bytes memory init = abi.encodeCall(Vault.initialize, (canister, guardian));
        vault = Vault(payable(address(new ERC1967Proxy(address(impl), init))));

        deal(USDC, user, AMOUNT);
    }

    function _skipWithoutFork() internal {
        if (bytes(forkUrl).length == 0) vm.skip(true);
    }

    function _signAuthorization(uint256 key, uint256 validAfter, uint256 validBefore, bytes32 nonce)
        internal
        view
        returns (Vault.Signature memory)
    {
        bytes32 structHash =
            keccak256(abi.encode(RECEIVE_TYPEHASH, user, address(vault), AMOUNT, validAfter, validBefore, nonce));
        bytes32 digest = keccak256(abi.encodePacked("\x19\x01", IERC20Permit(USDC).DOMAIN_SEPARATOR(), structHash));
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(key, digest);
        return Vault.Signature(v, r, s);
    }

    function _signPermit(uint256 key, address spender, uint256 nonce, uint256 deadline)
        internal
        view
        returns (uint8 v, bytes32 r, bytes32 s)
    {
        bytes32 structHash = keccak256(abi.encode(PERMIT_TYPEHASH, user, spender, AMOUNT, nonce, deadline));
        bytes32 digest = keccak256(abi.encodePacked("\x19\x01", IERC20Permit(USDC).DOMAIN_SEPARATOR(), structHash));
        (v, r, s) = vm.sign(key, digest);
    }

    // ---------------------------------------------------------------- EIP-2612

    function _acceptance(bytes32 quoteHash, uint256 deadline) internal view returns (Vault.QuoteAcceptance memory) {
        return
            Vault.QuoteAcceptance(quoteHash, USDC, user, AMOUNT, deadline, DST_CHAIN, DST_TOKEN, DST_ADDRESS, MIN_OUT);
    }

    function test_fork_2612_happy_path_against_real_usdc() public {
        _skipWithoutFork();

        uint256 deadline = block.timestamp + 1 hours;
        uint256 nonce = IERC20Permit(USDC).nonces(user);
        (uint8 v, bytes32 r, bytes32 s) = _signPermit(userKey, address(vault), nonce, deadline);
        Vault.QuoteAcceptance memory a = _acceptance("q1", deadline);
        bytes memory sig = _signFor(userKey, a, address(vault), block.chainid);

        vm.prank(canister);
        vault.pullWithPermit(a, sig, Vault.Signature(v, r, s));

        assertEq(IERC20(USDC).balanceOf(address(vault)), AMOUNT, "the pull landed");
        assertEq(IERC20(USDC).balanceOf(user), 0, "the user paid");
        assertEq(IERC20Permit(USDC).nonces(user), nonce + 1, "the vault's own permit spent the nonce");
        assertEq(IERC20(USDC).allowance(user, address(vault)), 0, "the permit's allowance was spent in full");
    }

    /// A griefer lands the identical permit on the real token first. Our own
    /// `permit` call then reverts inside USDC, the catch swallows it, and the
    /// pull lands on the owner's acceptance and the allowance the griefer set.
    function test_fork_2612_survives_a_real_front_run() public {
        _skipWithoutFork();

        uint256 deadline = block.timestamp + 1 hours;
        uint256 nonce = IERC20Permit(USDC).nonces(user);
        (uint8 v, bytes32 r, bytes32 s) = _signPermit(userKey, address(vault), nonce, deadline);
        Vault.QuoteAcceptance memory a = _acceptance("q1", deadline);
        bytes memory sig = _signFor(userKey, a, address(vault), block.chainid);

        vm.prank(stranger);
        IERC20Permit(USDC).permit(user, address(vault), AMOUNT, deadline, v, r, s);

        vm.prank(canister);
        vault.pullWithPermit(a, sig, Vault.Signature(v, r, s));

        assertEq(IERC20(USDC).balanceOf(address(vault)), AMOUNT, "the pull landed after the front-run");
        assertEq(IERC20Permit(USDC).nonces(user), nonce + 1, "exactly one nonce was spent");
    }

    /// The worst case against the real token: a standing maximum allowance and
    /// even a genuine permit for the vault, under an acceptance signed by
    /// somebody else. It is refused before the vault touches USDC at all.
    function test_fork_2612_forged_acceptance_is_refused_against_real_usdc() public {
        _skipWithoutFork();

        vm.prank(user);
        IERC20(USDC).approve(address(vault), type(uint256).max); // the worst case

        uint256 deadline = block.timestamp + 1 hours;
        uint256 nonce = IERC20Permit(USDC).nonces(user);
        (uint8 v, bytes32 r, bytes32 s) = _signPermit(userKey, address(vault), nonce, deadline);
        Vault.QuoteAcceptance memory a = _acceptance("q1", deadline);
        bytes memory forged = _signFor(strangerKey, a, address(vault), block.chainid);

        vm.startStateDiffRecording();
        vm.prank(canister);
        vm.expectRevert(Vault.QuoteNotAccepted.selector);
        vault.pullWithPermit(a, forged, Vault.Signature(v, r, s));
        Vm.AccountAccess[] memory accesses = vm.stopAndReturnStateDiff();
        for (uint256 i = 0; i < accesses.length; i++) {
            assertTrue(accesses[i].account != USDC, "USDC was called before the acceptance was checked");
        }
    }

    // ---------------------------------------------------------------- EIP-3009

    function test_fork_3009_pull_with_the_quote_hash_as_the_nonce() public {
        _skipWithoutFork();

        uint256 validBefore = block.timestamp + 1 hours;
        Vault.Signature memory sig = _signAuthorization(userKey, 0, validBefore, "q1");
        vm.prank(canister);
        vault.pullWithAuthorization("q1", USDC, user, AMOUNT, 0, validBefore, sig);

        assertEq(IERC20(USDC).balanceOf(address(vault)), AMOUNT, "the pull landed");
        assertEq(IERC20(USDC).balanceOf(user), 0, "the user paid");
        assertTrue(IAuthorizationState(USDC).authorizationState(user, "q1"), "the token burned the quote hash");
        // the door's whole claim: no standing authorization is left behind
        assertEq(IERC20(USDC).allowance(user, address(vault)), 0, "an allowance was created");
    }

    function test_fork_3009_cannot_be_front_run_by_anyone() public {
        _skipWithoutFork();

        uint256 validBefore = block.timestamp + 1 hours;
        Vault.Signature memory sig = _signAuthorization(userKey, 0, validBefore, "q1");

        vm.prank(stranger);
        vm.expectRevert("FiatTokenV2: caller must be the payee");
        IERC3009(USDC).receiveWithAuthorization(user, address(vault), AMOUNT, 0, validBefore, "q1", sig.v, sig.r, sig.s);

        vm.prank(canister);
        vault.pullWithAuthorization("q1", USDC, user, AMOUNT, 0, validBefore, sig);
        assertEq(IERC20(USDC).balanceOf(address(vault)), AMOUNT, "the payee's own pull still lands");
    }

    function test_fork_3009_forged_signature_is_refused() public {
        _skipWithoutFork();

        vm.prank(user);
        IERC20(USDC).approve(address(vault), type(uint256).max); // the worst case

        uint256 validBefore = block.timestamp + 1 hours;
        Vault.Signature memory forged = _signAuthorization(strangerKey, 0, validBefore, "q1");
        vm.prank(canister);
        vm.expectRevert("FiatTokenV2: invalid signature");
        vault.pullWithAuthorization("q1", USDC, user, AMOUNT, 0, validBefore, forged);

        assertEq(IERC20(USDC).balanceOf(address(vault)), 0, "a standing allowance moved nothing");
    }

    /// Circle's bounds are strict on both sides, so the canister's window has to
    /// leave a second at each end. Equality reverts on chain, not in the canister.
    function test_fork_3009_window_bounds_are_strict() public {
        _skipWithoutFork();

        // every signature is taken into a local first: an inline helper call in
        // an argument position staticcalls the token and spends the prank
        uint256 now_ = block.timestamp;
        Vault.Signature memory afterEq = _signAuthorization(userKey, now_, now_ + 1 hours, "q1");
        Vault.Signature memory beforeEq = _signAuthorization(userKey, 0, now_, "q2");
        Vault.Signature memory inside = _signAuthorization(userKey, now_ - 1, now_ + 1, "q3");

        vm.prank(canister);
        vm.expectRevert("FiatTokenV2: authorization is not yet valid");
        vault.pullWithAuthorization("q1", USDC, user, AMOUNT, now_, now_ + 1 hours, afterEq);

        vm.prank(canister);
        vm.expectRevert("FiatTokenV2: authorization is expired");
        vault.pullWithAuthorization("q2", USDC, user, AMOUNT, 0, now_, beforeEq);

        vm.prank(canister);
        vault.pullWithAuthorization("q3", USDC, user, AMOUNT, now_ - 1, now_ + 1, inside);
        assertEq(IERC20(USDC).balanceOf(address(vault)), AMOUNT, "one second either side works");
    }
}
