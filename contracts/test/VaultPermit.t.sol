// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "forge-std/Test.sol";
import "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import "../src/Vault.sol";
import "./mocks/PermitToken.sol";
import "./mocks/AuthorizationToken.sol";
import "./mocks/PhantomPermitToken.sol";
import "./mocks/ContractWalletMock.sol";
import "./mocks/BatchOnlyDelegate.sol";
import "./mocks/TestToken.sol";
import "./helpers/AcceptanceSigner.sol";

/// The EIP-2612 door, on the pattern Across's SpokePoolPeriphery and 1inch's
/// OrderMixin use: the swallowed permit carries no authority at all. What
/// authorizes the pull is the owner's EIP-712 QuoteAcceptance over this
/// vault's own domain, verified before the permit and before the pull. The
/// permit is only a way to get the allowance there without the user paying
/// gas, and whether it lands, was front-run, or was never a permit at all
/// changes nothing about who may be pulled.
///
/// Every digest here is built by AcceptanceSigner from the literal type string
/// and domain values, never read from the vault. Every signature is taken into
/// a local before the prank that submits it: an inline helper that calls out
/// would spend the prank.
contract VaultPermitTest is AcceptanceSigner {
    Vault vault;
    PermitToken permitToken;
    AuthorizationToken feeToken;
    PhantomPermitToken phantom;
    TestToken plainToken;

    address canister = address(0xCA);
    address guardian = address(0x6A);
    uint256 userKey = 0xA11CE;
    address user = vm.addr(userKey);
    uint256 strangerKey = 0xBADBEEF;
    address other = address(0x0F);

    function setUp() public {
        vault = _newVault();

        permitToken = new PermitToken();
        feeToken = new AuthorizationToken();
        phantom = new PhantomPermitToken();
        plainToken = new TestToken();

        permitToken.transfer(user, 1000e18);
        feeToken.transfer(user, 1000e18);
        phantom.transfer(user, 1000e18);
        plainToken.transfer(user, 1000e18);

        vm.warp(1_700_000_000);
    }

    function _newVault() internal returns (Vault) {
        Vault impl = new Vault();
        bytes memory init = abi.encodeCall(Vault.initialize, (canister, guardian));
        return Vault(payable(address(new ERC1967Proxy(address(impl), init))));
    }

    // ------------------------------------------------------------------- helpers

    function _acceptance(bytes32 quoteHash, address token, uint256 amount, uint256 deadline)
        internal
        view
        returns (Vault.QuoteAcceptance memory)
    {
        return
            Vault.QuoteAcceptance(quoteHash, token, user, amount, deadline, DST_CHAIN, DST_TOKEN, DST_ADDRESS, MIN_OUT);
    }

    /// the owner's acceptance of `a`, for this vault on this chain
    function _accept(Vault.QuoteAcceptance memory a) internal view returns (bytes memory) {
        return _signFor(userKey, a, address(vault), block.chainid);
    }

    function _signPermit(uint256 signerKey, address spender, uint256 value, uint256 deadline)
        internal
        view
        returns (Vault.Signature memory)
    {
        return _signPermitFor(IERC20Permit(address(permitToken)), signerKey, spender, value, deadline);
    }

    function _signPermitFor(IERC20Permit token, uint256 signerKey, address spender, uint256 value, uint256 deadline)
        internal
        view
        returns (Vault.Signature memory)
    {
        bytes32 structHash = keccak256(
            abi.encode(
                keccak256("Permit(address owner,address spender,uint256 value,uint256 nonce,uint256 deadline)"),
                user,
                spender,
                value,
                token.nonces(user),
                deadline
            )
        );
        bytes32 digest = keccak256(abi.encodePacked("\x19\x01", token.DOMAIN_SEPARATOR(), structHash));
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(signerKey, digest);
        return Vault.Signature(v, r, s);
    }

    function _garbagePermit() internal pure returns (Vault.Signature memory) {
        return Vault.Signature(27, bytes32(uint256(1)), bytes32(uint256(2)));
    }

    /// A refusal that is meant to come before any token call: the recorded
    /// state diff of the reverted pull must not show the token at all.
    function _assertTokenUntouched(Vm.AccountAccess[] memory accesses, address token) internal pure {
        for (uint256 i = 0; i < accesses.length; i++) {
            assertTrue(accesses[i].account != token, "the token was called before the acceptance was checked");
        }
    }

    function _refusedBeforeAnyTokenCall(
        Vault.QuoteAcceptance memory a,
        bytes memory acceptanceSig,
        Vault.Signature memory permit
    ) internal {
        vm.startStateDiffRecording();
        vm.prank(canister);
        vm.expectRevert(Vault.QuoteNotAccepted.selector);
        vault.pullWithPermit(a, acceptanceSig, permit);
        _assertTokenUntouched(vm.stopAndReturnStateDiff(), a.token);
    }

    // ------------------------------------------------------- the door still works

    function test_pull_with_permit_moves_funds_user_pays_nothing() public {
        uint256 deadline = block.timestamp + 1 hours;
        Vault.QuoteAcceptance memory a = _acceptance("q1", address(permitToken), 50e18, deadline);
        bytes memory sig = _accept(a);
        Vault.Signature memory permit = _signPermit(userKey, address(vault), 50e18, deadline);

        vm.expectEmit(address(vault));
        emit Vault.Deposited("q1", address(permitToken), user, 50e18);

        vm.prank(canister);
        vault.pullWithPermit(a, sig, permit);

        assertEq(permitToken.balanceOf(address(vault)), 50e18, "the pull landed");
        assertEq(permitToken.nonces(user), 1, "the vault's own permit spent the nonce");
        assertEq(permitToken.allowance(user, address(vault)), 0, "the permit's allowance was spent in full");
        assertTrue(vault.quoteKeyUsed("q1", user), "the quote is marked");
    }

    /// A permit is a bearer authorization: a griefer mines it first, our own
    /// `permit` call reverts, the catch swallows it, and the pull still lands on
    /// the owner's acceptance and the allowance the griefer set for us.
    function test_pull_survives_permit_front_run() public {
        uint256 deadline = block.timestamp + 1 hours;
        Vault.QuoteAcceptance memory a = _acceptance("q2", address(permitToken), 50e18, deadline);
        bytes memory sig = _accept(a);
        Vault.Signature memory permit = _signPermit(userKey, address(vault), 50e18, deadline);
        permitToken.permit(user, address(vault), 50e18, deadline, permit.v, permit.r, permit.s); // griefer

        vm.prank(canister);
        vault.pullWithPermit(a, sig, permit);
        assertEq(permitToken.balanceOf(address(vault)), 50e18, "the pull landed after the front-run");
    }

    function test_pull_only_canister() public {
        Vault.QuoteAcceptance memory a = _acceptance("q3", address(permitToken), 1, block.timestamp + 1 hours);
        bytes memory sig = _accept(a);
        vm.expectRevert(Vault.OnlyCanister.selector);
        vault.pullWithPermit(a, sig, _garbagePermit());
    }

    function test_blocked_when_deposits_paused() public {
        uint256 deadline = block.timestamp + 1 hours;
        Vault.QuoteAcceptance memory a = _acceptance("q4", address(permitToken), 50e18, deadline);
        bytes memory sig = _accept(a);
        Vault.Signature memory permit = _signPermit(userKey, address(vault), 50e18, deadline);

        vm.prank(guardian);
        vault.pause(Vault.PauseClass.Deposits);

        vm.prank(canister);
        vm.expectRevert(Vault.IsPaused.selector);
        vault.pullWithPermit(a, sig, permit);

        // the positive control: the same pull lands once the class is lifted, so
        // the refusal above was the pause and nothing else
        vm.prank(canister);
        vault.unpause(Vault.PauseClass.Deposits);
        vm.prank(canister);
        vault.pullWithPermit(a, sig, permit);
        assertEq(permitToken.balanceOf(address(vault)), 50e18, "the pull landed after the unpause");
    }

    /// EIP-2612 and OpenZeppelin both accept a permit in the very second of its
    /// deadline, so the vault's own check, which the acceptance shares, must agree.
    function test_a_permit_is_live_up_to_its_deadline_second() public {
        uint256 deadline = block.timestamp + 1 hours;
        Vault.QuoteAcceptance memory a = _acceptance("q1", address(permitToken), 50e18, deadline);
        bytes memory sig = _accept(a);
        Vault.Signature memory permit = _signPermit(userKey, address(vault), 50e18, deadline);

        vm.warp(deadline);
        vm.prank(canister);
        vault.pullWithPermit(a, sig, permit);
        assertEq(permitToken.balanceOf(address(vault)), 50e18, "the pull landed on the deadline second");
    }

    // ------------------------------------------------------ the acceptance decides

    /// The hole V3 closed by rebuilding the permit's digest, now closed by the
    /// acceptance instead: a standing allowance and a forged signature. It is
    /// refused before the vault touches the token at all.
    function test_a_forged_acceptance_cannot_move_a_standing_allowance() public {
        uint256 deadline = block.timestamp + 1 hours;
        vm.prank(user);
        permitToken.approve(address(vault), type(uint256).max);

        Vault.QuoteAcceptance memory a = _acceptance("q1", address(permitToken), 50e18, deadline);
        bytes memory forged = abi.encodePacked(bytes32(uint256(1)), bytes32(uint256(2)), uint8(27));
        _refusedBeforeAnyTokenCall(a, forged, _garbagePermit());
    }

    /// And with no acceptance at all, on a token with no 2612 either: nothing
    /// is asked of the token until the owner's signature has been checked.
    function test_a_missing_acceptance_is_refused_before_any_token_call() public {
        vm.prank(user);
        plainToken.approve(address(vault), type(uint256).max);

        Vault.QuoteAcceptance memory a = _acceptance("q1", address(plainToken), 50e18, block.timestamp + 1 hours);
        _refusedBeforeAnyTokenCall(a, "", _garbagePermit());
    }

    /// A genuine signature over exactly this acceptance, by the wrong key.
    function test_an_acceptance_signed_by_a_stranger_is_refused() public {
        vm.prank(user);
        permitToken.approve(address(vault), type(uint256).max);

        Vault.QuoteAcceptance memory a = _acceptance("q1", address(permitToken), 50e18, block.timestamp + 1 hours);
        bytes memory byStranger = _signFor(strangerKey, a, address(vault), block.chainid);
        _refusedBeforeAnyTokenCall(a, byStranger, _garbagePermit());
    }

    /// The domain carries this vault's address: an acceptance the owner made for
    /// another vault on the same chain (staging, say) is not one for this vault.
    function test_an_acceptance_for_another_vault_is_refused() public {
        Vault staging = _newVault();
        vm.prank(user);
        permitToken.approve(address(vault), type(uint256).max);

        Vault.QuoteAcceptance memory a = _acceptance("q1", address(permitToken), 50e18, block.timestamp + 1 hours);
        bytes memory forStaging = _signFor(userKey, a, address(staging), block.chainid);
        _refusedBeforeAnyTokenCall(a, forStaging, _garbagePermit());
    }

    /// And its chain: the same vault address on another chain gets nothing from
    /// an acceptance made for this one. The domain is computed per call, so it
    /// follows `block.chainid` rather than whatever chain the vault was born on.
    function test_an_acceptance_from_another_chain_is_refused() public {
        vm.prank(user);
        permitToken.approve(address(vault), type(uint256).max);

        Vault.QuoteAcceptance memory a = _acceptance("q1", address(permitToken), 50e18, block.timestamp + 1 hours);
        bytes memory here = _accept(a);

        vm.chainId(8453);
        _refusedBeforeAnyTokenCall(a, here, _garbagePermit());

        // the control: signed for the chain the vault is on now, it lands
        bytes memory there = _signFor(userKey, a, address(vault), 8453);
        vm.prank(canister);
        vault.pullWithPermit(a, there, _garbagePermit());
        assertEq(permitToken.balanceOf(address(vault)), 50e18, "the acceptance for this chain landed");
    }

    function test_an_acceptance_for_another_quote_is_refused() public {
        vm.prank(user);
        permitToken.approve(address(vault), type(uint256).max);
        uint256 deadline = block.timestamp + 1 hours;

        bytes memory forA = _accept(_acceptance("qA", address(permitToken), 50e18, deadline));
        _refusedBeforeAnyTokenCall(_acceptance("qB", address(permitToken), 50e18, deadline), forA, _garbagePermit());
    }

    function test_an_acceptance_for_another_amount_is_refused() public {
        vm.prank(user);
        permitToken.approve(address(vault), type(uint256).max);
        uint256 deadline = block.timestamp + 1 hours;

        bytes memory for50 = _accept(_acceptance("q1", address(permitToken), 50e18, deadline));
        _refusedBeforeAnyTokenCall(_acceptance("q1", address(permitToken), 40e18, deadline), for50, _garbagePermit());
    }

    function test_an_acceptance_for_another_token_is_refused() public {
        vm.prank(user);
        plainToken.approve(address(vault), type(uint256).max);
        uint256 deadline = block.timestamp + 1 hours;

        bytes memory forPermitToken = _accept(_acceptance("q1", address(permitToken), 50e18, deadline));
        _refusedBeforeAnyTokenCall(
            _acceptance("q1", address(plainToken), 50e18, deadline), forPermitToken, _garbagePermit()
        );
    }

    function test_an_acceptance_for_another_deadline_is_refused() public {
        vm.prank(user);
        permitToken.approve(address(vault), type(uint256).max);
        uint256 deadline = block.timestamp + 1 hours;

        bytes memory sig = _accept(_acceptance("q1", address(permitToken), 50e18, deadline));
        _refusedBeforeAnyTokenCall(_acceptance("q1", address(permitToken), 50e18, deadline + 1), sig, _garbagePermit());
    }

    /// The readable economics are what a wallet shows the owner, and they are
    /// signed: the canister cannot swap in another destination chain, token,
    /// address or floor behind an acceptance of the quote the owner read.
    function test_an_acceptance_with_other_economics_is_refused() public {
        vm.prank(user);
        permitToken.approve(address(vault), type(uint256).max);

        Vault.QuoteAcceptance memory a = _acceptance("q1", address(permitToken), 50e18, block.timestamp + 1 hours);
        bytes memory sig = _accept(a);

        Vault.QuoteAcceptance memory b = _acceptance("q1", address(permitToken), 50e18, block.timestamp + 1 hours);
        b.dstChainId = 8453;
        _refusedBeforeAnyTokenCall(b, sig, _garbagePermit());

        b = _acceptance("q1", address(permitToken), 50e18, block.timestamp + 1 hours);
        b.dstToken = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913";
        _refusedBeforeAnyTokenCall(b, sig, _garbagePermit());

        b = _acceptance("q1", address(permitToken), 50e18, block.timestamp + 1 hours);
        b.dstAddress = "7EcDhSYGxXyscszYEp35KHN8vvw3svAuLKTzXwCFLtV"; // a Solana address
        _refusedBeforeAnyTokenCall(b, sig, _garbagePermit());

        b = _acceptance("q1", address(permitToken), 50e18, block.timestamp + 1 hours);
        b.minOut = MIN_OUT - 1;
        _refusedBeforeAnyTokenCall(b, sig, _garbagePermit());

        // the control: the acceptance as signed lands
        vm.prank(canister);
        vault.pullWithPermit(a, sig, _garbagePermit());
        assertEq(permitToken.balanceOf(address(vault)), 50e18, "the acceptance as signed landed");
    }

    /// The deadline is checked first, and the acceptance shares it: a genuine
    /// acceptance, a genuine permit consumed while live, and the allowance it
    /// left behind are all refused once the deadline has passed.
    function test_expired_permit_with_standing_allowance_is_refused() public {
        uint256 deadline = block.timestamp + 1 hours;
        Vault.QuoteAcceptance memory a = _acceptance("q1", address(permitToken), 50e18, deadline);
        bytes memory sig = _accept(a);
        Vault.Signature memory permit = _signPermit(userKey, address(vault), 50e18, deadline);
        permitToken.permit(user, address(vault), 50e18, deadline, permit.v, permit.r, permit.s); // consumed while valid
        assertEq(permitToken.allowance(user, address(vault)), 50e18, "the allowance it left");

        vm.warp(deadline + 1);
        vm.prank(canister);
        vm.expectRevert(Vault.PermitExpired.selector);
        vault.pullWithPermit(a, sig, permit);
    }

    /// "Before anything" is literal: an expired deadline is what the door reports
    /// even when the quote key is already spent.
    function test_the_deadline_is_checked_before_the_quote_key() public {
        uint256 deadline = block.timestamp + 1 hours;
        Vault.QuoteAcceptance memory a = _acceptance("q1", address(permitToken), 50e18, deadline);
        bytes memory sig = _accept(a);
        Vault.Signature memory permit = _signPermit(userKey, address(vault), 50e18, deadline);
        vm.prank(canister);
        vault.pullWithPermit(a, sig, permit);

        vm.warp(deadline + 1);
        vm.prank(canister);
        vm.expectRevert(Vault.PermitExpired.selector);
        vault.pullWithPermit(a, sig, permit);
    }

    // ------------------------------------------------------- both residuals, closed

    /// The correctness review's F2, and the reason V3 is gone: a permit the
    /// vault itself already consumed, replayed under new quotes against a
    /// standing allowance the owner granted later. The permit carries no
    /// authority, so replaying it buys nothing: every new quote needs the
    /// owner's acceptance of that quote, and the old one is spent.
    function test_a_consumed_permit_replayed_under_new_quotes_is_refused() public {
        uint256 deadline = block.timestamp + 1 hours;
        Vault.QuoteAcceptance memory a = _acceptance("qA", address(permitToken), 50e18, deadline);
        bytes memory sigA = _accept(a);
        Vault.Signature memory permit = _signPermit(userKey, address(vault), 50e18, deadline);
        vm.prank(canister);
        vault.pullWithPermit(a, sigA, permit);
        assertEq(permitToken.balanceOf(address(vault)), 50e18);

        // later the user grants a standing allowance for the legacy deposit door
        vm.prank(user);
        permitToken.approve(address(vault), type(uint256).max);

        // the same permit under new quotes, carried by the old acceptance's signature
        _refusedBeforeAnyTokenCall(_acceptance("qB", address(permitToken), 50e18, deadline), sigA, permit);
        _refusedBeforeAnyTokenCall(_acceptance("qC", address(permitToken), 50e18, deadline), sigA, permit);
        // and under the old quote itself, which is spent
        vm.prank(canister);
        vm.expectRevert(Vault.QuoteHashUsed.selector);
        vault.pullWithPermit(a, sigA, permit);
    }

    /// Residual 2 of V3: a token whose `permit` returns without checking
    /// anything. Trusting a permit that returns meant trusting nothing, and a
    /// standing allowance was enough. The acceptance is checked first, so a
    /// phantom permit authorizes nothing.
    function test_a_phantom_permit_authorizes_nothing() public {
        vm.prank(user);
        phantom.approve(address(vault), type(uint256).max);

        Vault.QuoteAcceptance memory a = _acceptance("q1", address(phantom), 50e18, block.timestamp + 1 hours);
        _refusedBeforeAnyTokenCall(a, "", _garbagePermit());
        bytes memory byStranger = _signFor(strangerKey, a, address(vault), block.chainid);
        _refusedBeforeAnyTokenCall(a, byStranger, _garbagePermit());
    }

    // --------------------------------------------- the permit is a convenience only

    /// V3 refused a permit with another one of the owner's mined after it,
    /// because it rebuilt the digest for the nonce just spent. The permit no
    /// longer authorizes anything, so nothing is rebuilt: the allowance the
    /// front-run left is spent on the owner's acceptance, and the user does not
    /// have to sign again.
    function test_a_permit_with_another_mined_after_it_still_lands() public {
        uint256 deadline = block.timestamp + 1 hours;
        Vault.QuoteAcceptance memory a = _acceptance("q1", address(permitToken), 50e18, deadline);
        bytes memory sig = _accept(a);
        Vault.Signature memory permit = _signPermit(userKey, address(vault), 50e18, deadline);
        permitToken.permit(user, address(vault), 50e18, deadline, permit.v, permit.r, permit.s); // ours, nonce 0

        Vault.Signature memory elsewhere = _signPermit(userKey, other, 1e18, deadline);
        permitToken.permit(user, other, 1e18, deadline, elsewhere.v, elsewhere.r, elsewhere.s); // nonce 1

        vm.prank(canister);
        vault.pullWithPermit(a, sig, permit);
        assertEq(permitToken.balanceOf(address(vault)), 50e18, "the pull landed");
    }

    /// V3's zero-nonce shortcut is gone with the nonce arithmetic. A fresh owner
    /// who already holds a standing allowance needs no permit at all: garbage
    /// permit fields are swallowed and the acceptance moves exactly its amount.
    function test_a_standing_allowance_with_garbage_permit_fields_lands_on_the_acceptance() public {
        assertEq(permitToken.nonces(user), 0, "a fresh owner");
        vm.prank(user);
        permitToken.approve(address(vault), type(uint256).max);

        Vault.QuoteAcceptance memory a = _acceptance("q1", address(permitToken), 50e18, block.timestamp + 1 hours);
        bytes memory sig = _accept(a);
        vm.prank(canister);
        vault.pullWithPermit(a, sig, _garbagePermit());

        assertEq(permitToken.balanceOf(address(vault)), 50e18, "the acceptance moved its amount");
        assertEq(permitToken.nonces(user), 0, "no permit was spent");
    }

    /// V3 read the token's `nonces()` and `DOMAIN_SEPARATOR()` and refused a
    /// token without them. Nothing reads them now: a token with no 2612 surface
    /// at all, a standing allowance and the owner's acceptance land.
    function test_the_vault_never_reads_the_tokens_domain_or_nonces() public {
        vm.prank(user);
        plainToken.approve(address(vault), type(uint256).max);

        Vault.QuoteAcceptance memory a = _acceptance("q1", address(plainToken), 50e18, block.timestamp + 1 hours);
        bytes memory sig = _accept(a);
        vm.prank(canister);
        vault.pullWithPermit(a, sig, _garbagePermit());

        assertEq(plainToken.balanceOf(address(vault)), 50e18, "the pull landed");
    }

    // --------------------------------------------------------- owners with code

    /// An EIP-7702 account has code (the delegation designator, and on a Pectra
    /// chain the delegate's code behind it), yet its own key is still its root
    /// authority. ECDSA is checked first, so the owner's own signature is
    /// accepted whatever the delegate says about ERC-1271.
    function test_an_eoa_with_code_is_accepted_on_its_own_signature() public {
        uint256 deadline = block.timestamp + 1 hours;
        address delegate = address(new BatchOnlyDelegate());

        // the designator itself, as this repo's pre-Pectra EVM sees it
        vm.etch(user, abi.encodePacked(hex"ef0100", delegate));
        assertEq(user.code.length, 23, "a delegated EOA");
        Vault.QuoteAcceptance memory a = _acceptance("q1", address(permitToken), 50e18, deadline);
        bytes memory sig = _accept(a);
        Vault.Signature memory permit = _signPermit(userKey, address(vault), 50e18, deadline);
        vm.prank(canister);
        vault.pullWithPermit(a, sig, permit);
        assertEq(permitToken.balanceOf(address(vault)), 50e18, "the designated EOA's key was accepted");

        // and the delegate's code behind it, as a Pectra chain runs the account
        vm.etch(user, delegate.code);
        Vault.QuoteAcceptance memory b = _acceptance("q2", address(permitToken), 50e18, deadline);
        bytes memory sigB = _accept(b);
        Vault.Signature memory permitB = _signPermit(userKey, address(vault), 50e18, deadline);
        vm.prank(canister);
        vault.pullWithPermit(b, sigB, permitB);
        assertEq(permitToken.balanceOf(address(vault)), 100e18, "a delegate without ERC-1271 changed nothing");
    }

    /// A contract wallet cannot make an ECDSA signature for its own address, so
    /// it speaks through ERC-1271, as it does to Permit2 and Across. It holds a
    /// standing allowance, since the token's permit cannot verify it either.
    function test_an_erc1271_wallet_is_accepted() public {
        ContractWalletMock wallet = new ContractWalletMock(vm.addr(0x5AFE), true);
        permitToken.transfer(address(wallet), 100e18);
        vm.prank(vm.addr(0x5AFE));
        wallet.approve(IERC20(address(permitToken)), address(vault), 50e18);

        Vault.QuoteAcceptance memory a = _acceptance("q1", address(permitToken), 50e18, block.timestamp + 1 hours);
        a.owner = address(wallet);
        bytes memory sig = _signFor(0x5AFE, a, address(vault), block.chainid);

        vm.prank(canister);
        vault.pullWithPermit(a, sig, _garbagePermit());
        assertEq(permitToken.balanceOf(address(vault)), 50e18, "the wallet's acceptance moved its amount");
    }

    /// And a wallet whose ERC-1271 says no is refused, whoever signed, before
    /// the vault asks anything of the token.
    function test_a_rejecting_erc1271_wallet_is_refused() public {
        ContractWalletMock wallet = new ContractWalletMock(vm.addr(0x5AFE), false);
        permitToken.transfer(address(wallet), 100e18);
        vm.prank(vm.addr(0x5AFE));
        wallet.approve(IERC20(address(permitToken)), address(vault), type(uint256).max);

        Vault.QuoteAcceptance memory a = _acceptance("q1", address(permitToken), 50e18, block.timestamp + 1 hours);
        a.owner = address(wallet);
        bytes memory sig = _signFor(0x5AFE, a, address(vault), block.chainid);
        _refusedBeforeAnyTokenCall(a, sig, _garbagePermit());
    }

    // --------------------------------------------------------- what a wallet signs

    /// The domain as EIP-5267 reports it, so a wallet can build it without
    /// trusting a frontend, and the digest a wallet computes from the JSON
    /// typed data the frontend will send, equal to the digest every test above
    /// signs and the vault verifies.
    function test_the_acceptance_is_what_a_wallet_signs() public view {
        (
            bytes1 fields,
            string memory name,
            string memory version,
            uint256 chainId,
            address verifyingContract,
            bytes32 salt,
            uint256[] memory extensions
        ) = vault.eip712Domain();
        assertEq(fields, hex"0f", "name, version, chainId and verifyingContract");
        assertEq(name, "Swapic Vault");
        assertEq(version, "1");
        assertEq(chainId, block.chainid);
        assertEq(verifyingContract, address(vault));
        assertEq(salt, bytes32(0));
        assertEq(extensions.length, 0);

        assertEq(keccak256(bytes(ACCEPTANCE_TYPE)), ACCEPTANCE_TYPEHASH, "the pinned typehash");

        Vault.QuoteAcceptance memory a = _acceptance(
            0x9f2c1b7a5e3d4c6b8a0f1e2d3c4b5a69788796a5b4c3d2e1f001122334455667,
            address(permitToken),
            50e18,
            1_700_003_600
        );
        string memory json = string.concat(
            '{"types":{"EIP712Domain":[{"name":"name","type":"string"},{"name":"version","type":"string"},',
            '{"name":"chainId","type":"uint256"},{"name":"verifyingContract","type":"address"}],',
            '"QuoteAcceptance":[{"name":"quoteHash","type":"bytes32"},{"name":"token","type":"address"},',
            '{"name":"owner","type":"address"},{"name":"amount","type":"uint256"},{"name":"deadline","type":"uint256"},',
            '{"name":"dstChainId","type":"uint256"},{"name":"dstToken","type":"string"},',
            '{"name":"dstAddress","type":"string"},{"name":"minOut","type":"uint256"}]},',
            '"primaryType":"QuoteAcceptance",'
        );
        json = string.concat(
            json,
            '"domain":{"name":"Swapic Vault","version":"1","chainId":',
            vm.toString(block.chainid),
            ',"verifyingContract":"',
            vm.toString(address(vault)),
            '"},'
        );
        json = string.concat(
            json,
            '"message":{"quoteHash":"',
            vm.toString(a.quoteHash),
            '","token":"',
            vm.toString(a.token),
            '","owner":"',
            vm.toString(a.owner),
            '","amount":"50000000000000000000","deadline":"1700003600","dstChainId":"42161",'
        );
        json =
            string.concat(json, '"dstToken":"', DST_TOKEN, '","dstAddress":"', DST_ADDRESS, '","minOut":"24900000"}}');

        assertEq(vm.eip712HashTypedData(json), _digestFor(a, address(vault), block.chainid), "a wallet's digest");
    }

    // ------------------------------------------------------- the rest of the door

    /// Every door reports what actually arrived, never what was asked for.
    function test_fee_on_transfer_deposit_reports_the_measured_delta() public {
        uint256 deadline = block.timestamp + 1 hours;
        Vault.QuoteAcceptance memory a = _acceptance("q1", address(feeToken), 50e18, deadline);
        bytes memory sig = _accept(a);
        Vault.Signature memory permit =
            _signPermitFor(IERC20Permit(address(feeToken)), userKey, address(vault), 50e18, deadline);
        feeToken.setFeeBps(100); // 1%

        vm.expectEmit(true, true, true, true);
        emit Vault.Deposited("q1", address(feeToken), user, 50e18 - 0.5e18);

        vm.prank(canister);
        vault.pullWithPermit(a, sig, permit);

        assertEq(feeToken.balanceOf(address(vault)), 50e18 - 0.5e18, "the fee was taken");
    }

    function test_quote_hash_cannot_be_reused() public {
        uint256 deadline = block.timestamp + 1 hours;
        Vault.QuoteAcceptance memory a = _acceptance("q1", address(permitToken), 50e18, deadline);
        bytes memory sig = _accept(a);
        Vault.Signature memory permit = _signPermit(userKey, address(vault), 50e18, deadline);

        vm.prank(canister);
        vault.pullWithPermit(a, sig, permit);

        Vault.Signature memory permit2 = _signPermit(userKey, address(vault), 50e18, deadline);
        vm.prank(canister);
        vm.expectRevert(Vault.QuoteHashUsed.selector);
        vault.pullWithPermit(a, sig, permit2);
    }
}
