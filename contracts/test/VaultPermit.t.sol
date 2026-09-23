// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "forge-std/Test.sol";
import "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import "../src/Vault.sol";
import "./mocks/PermitToken.sol";
import "./mocks/AuthorizationToken.sol";
import "./mocks/DaiStylePermitToken.sol";
import "./mocks/NoncesOnlyToken.sol";
import "./mocks/TestToken.sol";

/// The EIP-2612 door. It keeps its try/catch, because a permit is a bearer
/// authorization anyone may submit and being front-run is not an attack. What
/// the catch branch may no longer do is shrug: it has to prove the permit the
/// token consumed was ours, by rebuilding the digest for the nonce just spent
/// and recovering the owner's own signature out of it.
///
/// Every refusal below that is meant to come from the rebuilt digest first
/// advances the owner's nonce, so it cannot be passing on the zero-nonce
/// shortcut instead. As in the 3009 suite, every signature is taken into a local
/// before the prank that submits it: the helpers staticcall the token, and a
/// call in an argument position would spend the prank.
contract VaultPermitTest is Test {
    Vault vault;
    PermitToken permitToken;
    AuthorizationToken feeToken;
    DaiStylePermitToken daiToken;
    NoncesOnlyToken halfToken;
    TestToken plainToken;

    address canister = address(0xCA);
    address guardian = address(0x6A);
    uint256 userKey = 0xA11CE;
    address user = vm.addr(userKey);
    uint256 strangerKey = 0xBADBEEF;
    address other = address(0x0F);

    function setUp() public {
        Vault impl = new Vault();
        bytes memory init = abi.encodeCall(Vault.initialize, (canister, guardian));
        vault = Vault(payable(address(new ERC1967Proxy(address(impl), init))));

        permitToken = new PermitToken();
        feeToken = new AuthorizationToken();
        daiToken = new DaiStylePermitToken();
        halfToken = new NoncesOnlyToken();
        plainToken = new TestToken();

        permitToken.transfer(user, 1000e18);
        feeToken.transfer(user, 1000e18);
        daiToken.transfer(user, 1000e18);
        halfToken.transfer(user, 1000e18);
        plainToken.transfer(user, 1000e18);

        vm.warp(1_700_000_000);
    }

    function signPermit(uint256 signerKey, address spender, uint256 value, uint256 deadline)
        internal
        view
        returns (uint8 v, bytes32 r, bytes32 s)
    {
        return signPermitFor(permitToken, signerKey, spender, value, permitToken.nonces(user), deadline);
    }

    function signPermitFor(
        IERC20Permit token,
        uint256 signerKey,
        address spender,
        uint256 value,
        uint256 nonce,
        uint256 deadline
    ) internal view returns (uint8 v, bytes32 r, bytes32 s) {
        bytes32 structHash = keccak256(
            abi.encode(
                keccak256("Permit(address owner,address spender,uint256 value,uint256 nonce,uint256 deadline)"),
                user,
                spender,
                value,
                nonce,
                deadline
            )
        );
        bytes32 digest = keccak256(abi.encodePacked("\x19\x01", token.DOMAIN_SEPARATOR(), structHash));
        (v, r, s) = vm.sign(signerKey, digest);
    }

    /// A genuine permit of the user's to somebody else, mined: the nonce moves
    /// on by one and the vault's allowance is untouched.
    function _spendANonceElsewhere() internal {
        uint256 deadline = block.timestamp + 1 hours;
        (uint8 v, bytes32 r, bytes32 s) = signPermit(userKey, other, 1e18, deadline);
        permitToken.permit(user, other, 1e18, deadline, v, r, s);
    }

    // ------------------------------------------------------- the door still works

    function test_pull_with_permit_moves_funds_user_pays_nothing() public {
        uint256 deadline = block.timestamp + 1 hours;
        (uint8 v, bytes32 r, bytes32 s) = signPermit(userKey, address(vault), 50e18, deadline);
        vm.prank(canister);
        vault.pullWithPermit("q1", address(permitToken), user, 50e18, deadline, v, r, s);
        assertEq(permitToken.balanceOf(address(vault)), 50e18);
    }

    /// The defence the try/catch exists for, and the one line of it that must
    /// keep working after the fix: a griefer mines the exact permit first, and
    /// the pull still lands because the catch branch can prove it was ours.
    function test_pull_survives_permit_front_run() public {
        uint256 deadline = block.timestamp + 1 hours;
        (uint8 v, bytes32 r, bytes32 s) = signPermit(userKey, address(vault), 50e18, deadline);
        permitToken.permit(user, address(vault), 50e18, deadline, v, r, s); // griefer consumes it first
        vm.prank(canister);
        vault.pullWithPermit("q2", address(permitToken), user, 50e18, deadline, v, r, s); // must still work
        assertEq(permitToken.balanceOf(address(vault)), 50e18);
    }

    function test_pull_only_canister() public {
        vm.expectRevert(Vault.OnlyCanister.selector);
        vault.pullWithPermit("q3", address(permitToken), user, 1, 0, 0, 0, 0);
    }

    function test_blocked_when_deposits_paused() public {
        uint256 deadline = block.timestamp + 1 hours;
        (uint8 v, bytes32 r, bytes32 s) = signPermit(userKey, address(vault), 50e18, deadline);

        vm.prank(guardian);
        vault.pause(Vault.PauseClass.Deposits);

        vm.prank(canister);
        vm.expectRevert(Vault.IsPaused.selector);
        vault.pullWithPermit("q4", address(permitToken), user, 50e18, deadline, v, r, s);

        // the positive control: the same pull lands once the class is lifted, so
        // the refusal above was the pause and nothing else
        vm.prank(canister);
        vault.unpause(Vault.PauseClass.Deposits);
        vm.prank(canister);
        vault.pullWithPermit("q4", address(permitToken), user, 50e18, deadline, v, r, s);
        assertEq(permitToken.balanceOf(address(vault)), 50e18, "the pull landed after the unpause");
    }

    /// EIP-2612 and OpenZeppelin both accept a permit in the very second of its
    /// deadline, so the vault's own check must agree or it refuses a live one.
    function test_a_permit_is_live_up_to_its_deadline_second() public {
        uint256 deadline = block.timestamp + 1 hours;
        (uint8 v, bytes32 r, bytes32 s) = signPermit(userKey, address(vault), 50e18, deadline);

        vm.warp(deadline);
        vm.prank(canister);
        vault.pullWithPermit("q1", address(permitToken), user, 50e18, deadline, v, r, s);
        assertEq(permitToken.balanceOf(address(vault)), 50e18, "the pull landed on the deadline second");
    }

    // -------------------------------------------------------------- the hole, closed

    /// The hole this task closes. Before the fix the catch branch swallowed any
    /// signature at all and the transferFrom that followed moved the owner's
    /// funds on the strength of a standing allowance, which every user of the
    /// legacy deposit path holds. A forged (v, r, s) must now be refused.
    function test_forged_permit_cannot_move_a_standing_allowance() public {
        uint256 deadline = block.timestamp + 1 hours;
        _spendANonceElsewhere();
        assertEq(permitToken.nonces(user), 1, "the refusal must come from the digest, not the zero nonce");

        vm.prank(user);
        permitToken.approve(address(vault), type(uint256).max);

        vm.prank(canister);
        vm.expectRevert(Vault.PermitNotAuthorized.selector);
        vault.pullWithPermit(
            "q1", address(permitToken), user, 50e18, deadline, 27, bytes32(uint256(1)), bytes32(uint256(2))
        );

        assertEq(permitToken.balanceOf(address(vault)), 0, "a forged permit moved funds");
        // a permit approves and never transfers, so the user still holds it all
        assertEq(permitToken.balanceOf(user), 1000e18, "the user kept every token");
    }

    /// The same hole reached with a genuine signature made by the wrong key, over
    /// exactly the fields the catch branch rebuilds: nonce 0, this vault, this
    /// amount and deadline. The signer is the only thing wrong with it.
    function test_permit_signed_by_a_stranger_is_refused() public {
        uint256 deadline = block.timestamp + 1 hours;
        (uint8 v, bytes32 r, bytes32 s) = signPermit(strangerKey, address(vault), 50e18, deadline);
        _spendANonceElsewhere();

        vm.prank(user);
        permitToken.approve(address(vault), type(uint256).max);

        vm.prank(canister);
        vm.expectRevert(Vault.PermitNotAuthorized.selector);
        vault.pullWithPermit("q1", address(permitToken), user, 50e18, deadline, v, r, s);

        assertEq(permitToken.balanceOf(address(vault)), 0, "a stranger's signature moved funds");
    }

    function test_permit_signed_for_another_spender_is_refused() public {
        uint256 deadline = block.timestamp + 1 hours;
        (uint8 v, bytes32 r, bytes32 s) = signPermit(userKey, other, 50e18, deadline);
        permitToken.permit(user, other, 50e18, deadline, v, r, s); // genuine, but not for us

        vm.prank(user);
        permitToken.approve(address(vault), type(uint256).max);

        vm.prank(canister);
        vm.expectRevert(Vault.PermitNotAuthorized.selector);
        vault.pullWithPermit("q1", address(permitToken), user, 50e18, deadline, v, r, s);

        assertEq(permitToken.balanceOf(address(vault)), 0, "another spender's permit moved funds");
    }

    function test_permit_for_another_amount_is_refused() public {
        uint256 deadline = block.timestamp + 1 hours;
        (uint8 v, bytes32 r, bytes32 s) = signPermit(userKey, address(vault), 50e18, deadline);
        permitToken.permit(user, address(vault), 50e18, deadline, v, r, s);

        vm.prank(canister);
        vm.expectRevert(Vault.PermitNotAuthorized.selector);
        vault.pullWithPermit("q1", address(permitToken), user, 40e18, deadline, v, r, s);

        assertEq(permitToken.balanceOf(address(vault)), 0, "a permit for another amount moved funds");
    }

    function test_permit_for_another_deadline_is_refused() public {
        uint256 deadline = block.timestamp + 1 hours;
        (uint8 v, bytes32 r, bytes32 s) = signPermit(userKey, address(vault), 50e18, deadline);
        permitToken.permit(user, address(vault), 50e18, deadline, v, r, s);

        vm.prank(canister);
        vm.expectRevert(Vault.PermitNotAuthorized.selector);
        vault.pullWithPermit("q1", address(permitToken), user, 50e18, deadline + 1, v, r, s);

        assertEq(permitToken.balanceOf(address(vault)), 0, "a permit for another deadline moved funds");
    }

    /// The guard for the deadline line. Without it this permit still rebuilds to
    /// the owner in the catch branch, so the digest check alone would let an
    /// expired authorization spend the allowance it left behind.
    function test_expired_permit_with_standing_allowance_is_refused() public {
        uint256 deadline = block.timestamp + 1 hours;
        (uint8 v, bytes32 r, bytes32 s) = signPermit(userKey, address(vault), 50e18, deadline);
        permitToken.permit(user, address(vault), 50e18, deadline, v, r, s); // consumed while valid
        assertEq(permitToken.allowance(user, address(vault)), 50e18, "the allowance it left");

        vm.warp(deadline + 1);
        vm.prank(canister);
        vm.expectRevert(Vault.PermitExpired.selector);
        vault.pullWithPermit("q1", address(permitToken), user, 50e18, deadline, v, r, s);

        assertEq(permitToken.balanceOf(address(vault)), 0, "an expired permit moved funds");
    }

    /// "Before anything" is literal: an expired deadline is what the door reports
    /// even when the quote key is already spent, so the error names the refusal
    /// that would have stopped the pull on its own.
    function test_the_deadline_is_checked_before_the_quote_key() public {
        uint256 deadline = block.timestamp + 1 hours;
        (uint8 v, bytes32 r, bytes32 s) = signPermit(userKey, address(vault), 50e18, deadline);
        vm.prank(canister);
        vault.pullWithPermit("q1", address(permitToken), user, 50e18, deadline, v, r, s);

        vm.warp(deadline + 1);
        vm.prank(canister);
        vm.expectRevert(Vault.PermitExpired.selector);
        vault.pullWithPermit("q1", address(permitToken), user, 50e18, deadline, v, r, s);
    }

    /// The nonce is derived as nonces(owner) - 1, so a permit with another one in
    /// between is refused rather than accepted: strictly safer than taking the
    /// nonce as a parameter, at the cost of a revert the user retries.
    function test_a_second_permit_in_between_is_refused() public {
        uint256 deadline = block.timestamp + 1 hours;
        (uint8 v, bytes32 r, bytes32 s) = signPermit(userKey, address(vault), 50e18, deadline);
        permitToken.permit(user, address(vault), 50e18, deadline, v, r, s); // ours, nonce 0

        (uint8 v2, bytes32 r2, bytes32 s2) = signPermit(userKey, other, 1e18, deadline);
        permitToken.permit(user, other, 1e18, deadline, v2, r2, s2); // someone else's, nonce 1

        vm.prank(canister);
        vm.expectRevert(Vault.PermitNotAuthorized.selector);
        vault.pullWithPermit("q1", address(permitToken), user, 50e18, deadline, v, r, s);

        assertEq(permitToken.balanceOf(address(vault)), 0, "a stale permit moved funds");
    }

    /// nonces(owner) - 1 on a fresh owner must not underflow into a panic.
    function test_zero_nonce_does_not_underflow() public {
        uint256 deadline = block.timestamp + 1 hours;
        assertEq(permitToken.nonces(user), 0, "a fresh owner");

        vm.prank(user);
        permitToken.approve(address(vault), type(uint256).max);

        vm.prank(canister);
        vm.expectRevert(Vault.PermitNotAuthorized.selector);
        vault.pullWithPermit(
            "q1", address(permitToken), user, 50e18, deadline, 27, bytes32(uint256(1)), bytes32(uint256(2))
        );

        assertEq(permitToken.balanceOf(address(vault)), 0, "nothing moved");
    }

    /// A token with neither getter (Base's USDbC, BSC's Binance-Peg USDC) fails
    /// closed with a named error at the first one, `nonces()`.
    function test_token_without_nonces_is_refused() public {
        uint256 deadline = block.timestamp + 1 hours;

        vm.prank(user);
        plainToken.approve(address(vault), type(uint256).max);

        vm.prank(canister);
        vm.expectRevert(Vault.NotAPermitToken.selector);
        vault.pullWithPermit(
            "q1", address(plainToken), user, 50e18, deadline, 27, bytes32(uint256(1)), bytes32(uint256(2))
        );

        assertEq(plainToken.balanceOf(address(vault)), 0, "nothing moved");
    }

    /// And a token that answers `nonces()` but not `DOMAIN_SEPARATOR()` fails
    /// closed at the second: the domain is never guessed.
    function test_token_without_domain_separator_is_refused() public {
        uint256 deadline = block.timestamp + 1 hours;
        halfToken.setNonce(user, 1);

        vm.prank(user);
        halfToken.approve(address(vault), type(uint256).max);

        vm.prank(canister);
        vm.expectRevert(Vault.NotAPermitToken.selector);
        vault.pullWithPermit(
            "q1", address(halfToken), user, 50e18, deadline, 27, bytes32(uint256(1)), bytes32(uint256(2))
        );

        assertEq(halfToken.balanceOf(address(vault)), 0, "nothing moved");
    }

    /// DAI's permit is a different standard. The door speaks one, and says so by
    /// refusing: no such function at the call, and the wrong typehash after it.
    function test_dai_style_permit_is_refused() public {
        uint256 deadline = block.timestamp + 1 hours;

        bytes32 digest = keccak256(
            abi.encodePacked(
                "\x19\x01",
                daiToken.DOMAIN_SEPARATOR(),
                keccak256(abi.encode(daiToken.PERMIT_TYPEHASH(), user, address(vault), uint256(0), deadline, true))
            )
        );
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(userKey, digest);
        daiToken.permit(user, address(vault), 0, deadline, true, v, r, s); // genuine, and consumed
        assertEq(daiToken.allowance(user, address(vault)), type(uint256).max, "DAI approves all or nothing");
        assertEq(daiToken.nonces(user), 1, "the DAI nonce advanced");

        vm.prank(canister);
        vm.expectRevert(Vault.PermitNotAuthorized.selector);
        vault.pullWithPermit("q1", address(daiToken), user, 50e18, deadline, v, r, s);

        assertEq(daiToken.balanceOf(address(vault)), 0, "an infinite DAI allowance moved nothing");
    }

    // ------------------------------------------------------- the rest of the door

    /// Every door reports what actually arrived, never what was asked for.
    function test_fee_on_transfer_deposit_reports_the_measured_delta() public {
        uint256 deadline = block.timestamp + 1 hours;
        (uint8 v, bytes32 r, bytes32 s) =
            signPermitFor(IERC20Permit(address(feeToken)), userKey, address(vault), 50e18, 0, deadline);
        feeToken.setFeeBps(100); // 1%

        vm.expectEmit(true, true, true, true);
        emit Vault.Deposited("q1", address(feeToken), user, 50e18 - 0.5e18);

        vm.prank(canister);
        vault.pullWithPermit("q1", address(feeToken), user, 50e18, deadline, v, r, s);

        assertEq(feeToken.balanceOf(address(vault)), 50e18 - 0.5e18, "the fee was taken");
    }

    function test_quote_hash_cannot_be_reused() public {
        uint256 deadline = block.timestamp + 1 hours;
        (uint8 v, bytes32 r, bytes32 s) = signPermit(userKey, address(vault), 50e18, deadline);

        vm.prank(canister);
        vault.pullWithPermit("q1", address(permitToken), user, 50e18, deadline, v, r, s);

        (uint8 v2, bytes32 r2, bytes32 s2) = signPermit(userKey, address(vault), 50e18, deadline);
        vm.prank(canister);
        vm.expectRevert(Vault.QuoteHashUsed.selector);
        vault.pullWithPermit("q1", address(permitToken), user, 50e18, deadline, v2, r2, s2);

        assertEq(permitToken.balanceOf(address(vault)), 50e18, "only one pull landed");
    }
}
