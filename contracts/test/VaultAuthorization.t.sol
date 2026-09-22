// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "forge-std/Test.sol";
import "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import "../src/Vault.sol";
import "./mocks/AuthorizationToken.sol";
import "./mocks/TestToken.sol";

/// The EIP-3009 door. There is much less to test here than on the 2612 door and
/// that is the point: the token checks the signature, the payee and the window,
/// and burns the nonce, so the vault adds nothing that could be got wrong.
///
/// Every signature is built into a local before the prank that submits it: the
/// helpers staticcall the token for its typehash and domain, and an inline call
/// in an argument position would spend the prank before the vault ever sees it.
contract VaultAuthorizationTest is Test {
    Vault vault;
    AuthorizationToken token;
    TestToken plain;

    address canister = address(0xCA);
    address guardian = address(0x6A);
    uint256 userKey = 0xA11CE;
    address user = vm.addr(userKey);
    uint256 strangerKey = 0xBADBEEF;
    address stranger = vm.addr(strangerKey);

    uint256 constant AMOUNT = 50e18;

    function setUp() public {
        Vault impl = new Vault();
        bytes memory init = abi.encodeCall(Vault.initialize, (canister, guardian));
        vault = Vault(payable(address(new ERC1967Proxy(address(impl), init))));

        token = new AuthorizationToken();
        plain = new TestToken();
        token.transfer(user, 1000e18);

        vm.warp(1_700_000_000); // so that a validAfter of now - 1 exists
    }

    function _sign(uint256 signerKey, uint256 value, uint256 validAfter, uint256 validBefore, bytes32 nonce)
        internal
        view
        returns (Vault.Signature memory)
    {
        bytes32 structHash = keccak256(
            abi.encode(
                token.RECEIVE_WITH_AUTHORIZATION_TYPEHASH(), user, address(vault), value, validAfter, validBefore, nonce
            )
        );
        bytes32 digest = keccak256(abi.encodePacked("\x19\x01", token.DOMAIN_SEPARATOR(), structHash));
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(signerKey, digest);
        return Vault.Signature(v, r, s);
    }

    function _auth(bytes32 nonce) internal view returns (Vault.Signature memory) {
        return _sign(userKey, AMOUNT, 0, block.timestamp + 1 hours, nonce);
    }

    /// The binding: the authorization's nonce IS the quote hash, so a signature
    /// made for one swap can never be spent under another, and the token keeps a
    /// replay guard of its own beside the vault's usedQuoteKey.
    function test_authorization_pull_binds_the_quote_hash_as_the_nonce() public {
        Vault.Signature memory sig = _auth("q1");
        uint256 validBefore = block.timestamp + 1 hours;

        vm.expectEmit(true, true, true, true);
        emit Vault.Deposited("q1", address(token), user, AMOUNT);

        vm.prank(canister);
        vault.pullWithAuthorization("q1", address(token), user, AMOUNT, 0, validBefore, sig);

        assertEq(token.balanceOf(address(vault)), AMOUNT, "the pull landed");
        assertTrue(token.authorizationState(user, "q1"), "the token burned the quote hash as its nonce");
        assertTrue(vault.quoteKeyUsed("q1", user), "the vault marked the quote");
        // the whole reason this door exists: nothing is left standing afterwards
        assertEq(token.allowance(user, address(vault)), 0, "the door created an allowance");
    }

    function test_authorization_for_another_quote_is_refused() public {
        // the user signed for quote qB; the canister tries to spend it under qA,
        // so the digest the token rebuilds carries the wrong nonce
        Vault.Signature memory sig = _auth("qB");
        uint256 validBefore = block.timestamp + 1 hours;

        vm.prank(canister);
        vm.expectRevert("FiatTokenV2: invalid signature");
        vault.pullWithAuthorization("qA", address(token), user, AMOUNT, 0, validBefore, sig);

        assertEq(token.balanceOf(address(vault)), 0, "nothing moved");
        assertFalse(token.authorizationState(user, "qB"), "the other quote's nonce is still open");
    }

    function test_authorization_for_another_amount_is_refused() public {
        Vault.Signature memory sig = _sign(userKey, AMOUNT, 0, block.timestamp + 1 hours, "q1");
        uint256 validBefore = block.timestamp + 1 hours;

        vm.prank(canister);
        vm.expectRevert("FiatTokenV2: invalid signature");
        vault.pullWithAuthorization("q1", address(token), user, AMOUNT + 1, 0, validBefore, sig);

        assertEq(token.balanceOf(address(vault)), 0, "nothing moved");
    }

    function test_authorization_signed_by_a_stranger_is_refused() public {
        Vault.Signature memory forged = _sign(strangerKey, AMOUNT, 0, block.timestamp + 1 hours, "q1");
        uint256 validBefore = block.timestamp + 1 hours;

        // the 2612 door's hole, tried here: the user even holds a standing allowance
        vm.prank(user);
        token.approve(address(vault), type(uint256).max);

        vm.prank(canister);
        vm.expectRevert("FiatTokenV2: invalid signature");
        vault.pullWithAuthorization("q1", address(token), user, AMOUNT, 0, validBefore, forged);

        assertEq(token.balanceOf(address(vault)), 0, "a standing allowance moved nothing");
    }

    /// The front-running defence, and it is not ours: the token refuses anyone
    /// who is not the payee, so an authorization made out to this vault is
    /// useless to a griefer who lifts it out of the mempool.
    function test_authorization_cannot_be_submitted_by_anyone_else() public {
        Vault.Signature memory sig = _auth("q1");
        uint256 validBefore = block.timestamp + 1 hours;

        vm.prank(stranger);
        vm.expectRevert("FiatTokenV2: caller must be the payee");
        token.receiveWithAuthorization(user, address(vault), AMOUNT, 0, validBefore, "q1", sig.v, sig.r, sig.s);

        // and the vault's own door is canister-only on top of that
        vm.prank(stranger);
        vm.expectRevert(Vault.OnlyCanister.selector);
        vault.pullWithAuthorization("q1", address(token), user, AMOUNT, 0, validBefore, sig);

        // the authorization is untouched, so the real pull still works
        vm.prank(canister);
        vault.pullWithAuthorization("q1", address(token), user, AMOUNT, 0, validBefore, sig);
        assertEq(token.balanceOf(address(vault)), AMOUNT, "the payee's own pull still lands");
    }

    /// Both of Circle's bounds are strict, so equality on either side reverts on
    /// chain. The canister has to pick the window with that in mind.
    function test_authorization_outside_its_window_is_refused() public {
        uint256 notYet = block.timestamp + 1 hours;
        Vault.Signature memory tooEarly = _sign(userKey, AMOUNT, notYet, notYet + 1 hours, "q1");
        vm.prank(canister);
        vm.expectRevert("FiatTokenV2: authorization is not yet valid");
        vault.pullWithAuthorization("q1", address(token), user, AMOUNT, notYet, notYet + 1 hours, tooEarly);

        uint256 gone = block.timestamp - 1;
        Vault.Signature memory tooLate = _sign(userKey, AMOUNT, 0, gone, "q2");
        vm.prank(canister);
        vm.expectRevert("FiatTokenV2: authorization is expired");
        vault.pullWithAuthorization("q2", address(token), user, AMOUNT, 0, gone, tooLate);

        // equality, on both sides
        uint256 now_ = block.timestamp;
        Vault.Signature memory afterEq = _sign(userKey, AMOUNT, now_, now_ + 1 hours, "q3");
        vm.prank(canister);
        vm.expectRevert("FiatTokenV2: authorization is not yet valid");
        vault.pullWithAuthorization("q3", address(token), user, AMOUNT, now_, now_ + 1 hours, afterEq);

        Vault.Signature memory beforeEq = _sign(userKey, AMOUNT, 0, now_, "q4");
        vm.prank(canister);
        vm.expectRevert("FiatTokenV2: authorization is expired");
        vault.pullWithAuthorization("q4", address(token), user, AMOUNT, 0, now_, beforeEq);

        // and one second either side of the boundary does work
        Vault.Signature memory inside = _sign(userKey, AMOUNT, now_ - 1, now_ + 1, "q5");
        vm.prank(canister);
        vault.pullWithAuthorization("q5", address(token), user, AMOUNT, now_ - 1, now_ + 1, inside);
        assertEq(token.balanceOf(address(vault)), AMOUNT, "inside the window it lands");
    }

    /// A token with no receiveWithAuthorization must revert, never fall through
    /// to some other way of moving the money.
    function test_a_token_without_3009_is_refused() public {
        plain.transfer(user, 100e18);
        vm.prank(user);
        plain.approve(address(vault), type(uint256).max);

        Vault.Signature memory sig = _auth("q1");
        uint256 validBefore = block.timestamp + 1 hours;

        vm.prank(canister);
        vm.expectRevert();
        vault.pullWithAuthorization("q1", address(plain), user, AMOUNT, 0, validBefore, sig);

        assertEq(plain.balanceOf(address(vault)), 0, "no silent fallthrough onto the allowance");
    }

    function test_quote_hash_cannot_be_reused() public {
        Vault.Signature memory sig = _auth("q1");
        uint256 validBefore = block.timestamp + 1 hours;

        vm.prank(canister);
        vault.pullWithAuthorization("q1", address(token), user, AMOUNT, 0, validBefore, sig);

        // the vault refuses it first; the token would refuse the same nonce too
        vm.prank(canister);
        vm.expectRevert(Vault.QuoteHashUsed.selector);
        vault.pullWithAuthorization("q1", address(token), user, AMOUNT, 0, validBefore, sig);

        assertEq(token.balanceOf(address(vault)), AMOUNT, "only one pull landed");
    }

    /// Every door reports what actually arrived, never what was asked for.
    function test_fee_on_transfer_deposit_reports_the_measured_delta() public {
        Vault.Signature memory sig = _auth("q1");
        uint256 validBefore = block.timestamp + 1 hours;
        token.setFeeBps(100); // 1%

        vm.expectEmit(true, true, true, true);
        emit Vault.Deposited("q1", address(token), user, AMOUNT - AMOUNT / 100);

        vm.prank(canister);
        vault.pullWithAuthorization("q1", address(token), user, AMOUNT, 0, validBefore, sig);

        assertEq(token.balanceOf(address(vault)), AMOUNT - AMOUNT / 100, "the fee was taken");
    }

    function test_blocked_when_deposits_paused() public {
        Vault.Signature memory sig = _auth("q1");
        uint256 validBefore = block.timestamp + 1 hours;

        vm.prank(guardian);
        vault.pause(Vault.PauseClass.Deposits);

        vm.prank(canister);
        vm.expectRevert(Vault.IsPaused.selector);
        vault.pullWithAuthorization("q1", address(token), user, AMOUNT, 0, validBefore, sig);

        assertFalse(vault.quoteKeyUsed("q1", user), "a paused door burns no quote");
    }
}
