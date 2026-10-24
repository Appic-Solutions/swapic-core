// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "forge-std/Test.sol";
import "../src/Swapic.sol";
import "../src/Runner.sol";
import "../src/CctpReceiver.sol";
import "../src/SwapicTypes.sol";
import {Envelope} from "../src/libraries/Envelope.sol";
import "./mocks/RailMocks.sol";
import "./mocks/TestToken.sol";
import "./mocks/AuthorizationToken.sol";
import "./mocks/Permit2Mock.sol";
import "./mocks/Wallets.sol";
import "./helpers/CctpPlan.sol";
import {FEE} from "./helpers/TestConstants.sol";
import {GoldenOrder} from "./OrderHash.t.sol";

abstract contract SwapicBase is Test {
    Swapic swapic;
    Runner runner;
    CctpReceiver receiver;
    AuthorizationToken usdc;
    TestToken weth;
    WethMock wrapped;
    CctpMessengerMock messenger;
    TransmitterMock transmitter;
    DexMock dex;

    uint256 userKey = 0xA11CE;
    address user = vm.addr(userKey);
    uint256 strangerKey = 0xBADBEEF;
    address stranger = vm.addr(strangerKey);
    address guardian = address(0x6A);
    address canister = address(0xCA);
    string constant WITNESS_TYPE =
        "OrderWitness witness)OrderWitness(bytes32 orderId)TokenPermissions(address token,uint256 amount)";

    function setUp() public virtual {
        vm.warp(1_790_000_000);
        usdc = new AuthorizationToken();
        weth = new TestToken();
        wrapped = new WethMock();
        messenger = new CctpMessengerMock();
        transmitter = new TransmitterMock(IERC20(address(usdc)));
        dex = new DexMock();
        swapic = new Swapic(
            address(usdc),
            address(transmitter),
            CctpPlan.TOKEN_MESSENGER,
            address(wrapped),
            address(0),
            address(this),
            guardian,
            canister
        );
        runner = Runner(payable(swapic.runner()));
        receiver = CctpReceiver(swapic.cctpReceiver());
        vm.etch(PERMIT2, address(new Permit2SigMock()).code);

        usdc.transfer(address(dex), 1e24);
        usdc.transfer(address(transmitter), 1e24);
        usdc.transfer(user, 1000e6);
        weth.transfer(address(dex), 1e24);
        weth.transfer(user, 10e18);
        vm.deal(address(dex), 1e24);
        vm.deal(user, 10 ether);
        vm.startPrank(user);
        usdc.approve(address(swapic), type(uint256).max);
        weth.approve(address(swapic), type(uint256).max);
        vm.stopPrank();
    }

    function _envelope(address recipient) internal view returns (bytes memory) {
        return Envelope.encode(
            Envelope.Data(
                3, keccak256("ref"), Action.Swap, uint64(block.timestamp + 1 hours), recipient, address(weth), 1, 0, 0, 0)
        );
    }

    function _feeStep() internal view returns (Step memory) {
        return Step(address(usdc), abi.encodeCall(IERC20.transfer, (canister, FEE)), address(0), 0, new Write[](0));
    }

    function _burnStep(uint256 min) internal view returns (Step memory) {
        return CctpPlan.burnStep(
            address(messenger), address(usdc), min, 3, address(receiver), address(receiver), 10_000, 1000, _envelope(user)
        );
    }

    /// USDC in, the canister fee, the CCTP burn of the rest.
    function _usdcOrder() internal view returns (Order memory o) {
        o.ref = keccak256("ref");
        o.payer = user;
        o.tokenIn = address(usdc);
        o.amountIn = 100e6;
        o.deadline = block.timestamp + 60;
        o.steps = new Step[](2);
        o.steps[0] = _feeStep();
        o.steps[1] = _burnStep(99e6);
    }

    function _wethOrder() internal view returns (Order memory o) {
        o = _usdcOrder();
        o.tokenIn = address(weth);
        o.amountIn = 1e18;
        o.steps = new Step[](3);
        o.steps[0] = Step(
            address(dex),
            abi.encodeCall(DexMock.swap, (address(weth), address(usdc), 1e18, 2500e6)),
            address(weth),
            1e18,
            new Write[](0)
        );
        o.steps[1] = _feeStep();
        o.steps[2] = _burnStep(2400e6);
    }

    function _sig(uint256 key, bytes32 digest) internal pure returns (bytes memory) {
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(key, digest);
        return abi.encodePacked(r, s, v);
    }

    function _auth3009(Order memory o) internal view returns (GaslessAuth memory a) {
        a.door = Door.Auth3009;
        a.validBefore = block.timestamp + 1 hours;
        bytes32 structHash = keccak256(
            abi.encode(
                usdc.RECEIVE_WITH_AUTHORIZATION_TYPEHASH(),
                user,
                address(swapic),
                o.amountIn,
                0,
                a.validBefore,
                swapic.orderId(o)
            )
        );
        a.signature = _sig(userKey, keccak256(abi.encodePacked("\x19\x01", usdc.DOMAIN_SEPARATOR(), structHash)));
    }

    function _auth2612(Order memory o, uint256 acceptanceKey) internal view returns (GaslessAuth memory a) {
        a.door = Door.Permit2612;
        bytes32 permitDigest = keccak256(
            abi.encodePacked(
                "\x19\x01",
                usdc.DOMAIN_SEPARATOR(),
                keccak256(
                    abi.encode(
                        keccak256("Permit(address owner,address spender,uint256 value,uint256 nonce,uint256 deadline)"),
                        o.payer,
                        address(swapic),
                        o.amountIn,
                        usdc.nonces(o.payer),
                        o.deadline
                    )
                )
            )
        );
        a.signature = _sig(userKey, permitDigest);
        a.acceptance = _sig(acceptanceKey, swapic.acceptanceDigest(o));
    }

    function _permit2Sig(Order memory o, uint256 nonce, uint256 deadline) internal view returns (bytes memory) {
        bytes32 witness = keccak256(abi.encode(keccak256("OrderWitness(bytes32 orderId)"), swapic.orderId(o)));
        bytes32 d = Permit2SigMock(PERMIT2).digest(
            ISignatureTransfer.PermitTransferFrom(ISignatureTransfer.TokenPermissions(o.tokenIn, o.amountIn), nonce, deadline),
            address(swapic),
            witness,
            WITNESS_TYPE
        );
        return _sig(userKey, d);
    }

    function _authPermit2(Order memory o) internal view returns (GaslessAuth memory a) {
        a.door = Door.Permit2;
        a.nonce = 7;
        a.deadline = block.timestamp + 1 hours;
        a.signature = _permit2Sig(o, a.nonce, a.deadline);
    }

    function _assertEmpty() internal view {
        assertEq(usdc.balanceOf(address(runner)), 0);
        assertEq(weth.balanceOf(address(runner)), 0);
        assertEq(address(runner).balance, 0);
        assertEq(usdc.balanceOf(address(swapic)), 0);
        assertEq(weth.balanceOf(address(swapic)), 0);
        assertEq(address(swapic).balance, 0);
        assertEq(usdc.balanceOf(address(receiver)), 0);
    }
}

contract SwapicTest is SwapicBase {
    function test_constructor_deploys_receiver_then_runner() public view {
        assertEq(address(receiver), vm.computeCreateAddress(address(swapic), 1));
        assertEq(address(runner), vm.computeCreateAddress(address(swapic), 2));
        assertEq(runner.swapic(), address(swapic));
        assertEq(runner.cctpReceiver(), address(receiver));
        assertEq(runner.ckHelper(), address(0));
        assertEq(receiver.swapic(), address(swapic));
        assertEq(receiver.usdc(), address(usdc));
        assertEq(receiver.transmitter(), address(transmitter));
        assertEq(swapic.owner(), address(this));
        assertEq(swapic.canister(), canister);
        assertEq(swapic.guardian(), guardian);
    }

    function test_run_end_to_end() public {
        Order memory o = _wethOrder();
        bytes32 id = swapic.orderId(o);
        vm.expectEmit(address(swapic));
        emit ISwapic.OrderStarted(id, o.ref, user, address(weth), 1e18);
        vm.prank(user);
        assertEq(swapic.run(o), id);
        (uint256 burned,, bytes32 mintRecipient,, bytes32 caller,,,) = messenger.last();
        assertEq(burned, 2500e6 - FEE);
        assertEq(mintRecipient, bytes32(uint256(uint160(address(receiver)))));
        assertEq(caller, bytes32(uint256(uint160(address(receiver)))));
        assertEq(messenger.lastHookData(), _envelope(user));
        assertEq(usdc.balanceOf(canister), FEE);
        assertEq(weth.balanceOf(user), 9e18);
        assertTrue(swapic.used(id));
        _assertEmpty();
    }

    function test_run_native() public {
        Order memory o = _usdcOrder();
        o.tokenIn = NATIVE;
        o.amountIn = 1 ether;
        o.steps = new Step[](1);
        // built for the wrapped token: Swapic wraps the ether at the door
        o.steps[0] = Step(
            address(dex),
            abi.encodeCall(DexMock.swap, (address(wrapped), address(usdc), 1 ether, 2500e6)),
            address(wrapped),
            1 ether,
            new Write[](0)
        );
        o.payout = Payout(address(usdc), user, 2500e6);
        vm.prank(user);
        swapic.run{value: 1 ether}(o);
        assertEq(usdc.balanceOf(user), 1000e6 + 2500e6);
        assertEq(user.balance, 9 ether);
        assertEq(wrapped.balanceOf(address(dex)), 1 ether);
        assertEq(wrapped.balanceOf(address(runner)), 0);
        _assertEmpty();
    }

    function test_native_run_needs_the_exact_value() public {
        Order memory o = _usdcOrder();
        o.tokenIn = NATIVE;
        o.amountIn = 1 ether;
        vm.startPrank(user);
        vm.expectRevert(ISwapic.BadValue.selector);
        swapic.run{value: 1 ether - 1}(o);
        vm.expectRevert(ISwapic.BadValue.selector);
        swapic.run{value: 1 ether + 1}(o);
        Order memory e = _usdcOrder();
        vm.expectRevert(ISwapic.BadValue.selector);
        swapic.run{value: 1}(e);
        vm.stopPrank();
    }

    function test_native_is_refused_on_the_signature_doors() public {
        Order memory o = _usdcOrder();
        o.tokenIn = NATIVE;
        vm.prank(user);
        vm.expectRevert(ISwapic.BadValue.selector);
        swapic.runWithPermit2(o, Permit2Auth(0, 0, ""));
        GaslessAuth memory a;
        vm.prank(canister);
        vm.expectRevert(ISwapic.BadValue.selector);
        swapic.runFor(o, a);
    }

    function test_run_refuses_a_non_payer() public {
        Order memory o = _usdcOrder();
        vm.prank(stranger);
        vm.expectRevert(ISwapic.NotPayer.selector);
        swapic.run(o);
        vm.prank(stranger);
        vm.expectRevert(ISwapic.NotPayer.selector);
        swapic.runWithPermit2(o, Permit2Auth(0, 0, ""));
    }

    function test_run_for_refuses_a_non_canister() public {
        Order memory o = _usdcOrder();
        GaslessAuth memory a = _auth3009(o);
        vm.prank(user);
        vm.expectRevert(ISwapic.NotCanister.selector);
        swapic.runFor(o, a);
    }

    function test_expired_paused_reused() public {
        Order memory o = _usdcOrder();
        vm.warp(o.deadline + 1);
        vm.prank(user);
        vm.expectRevert(ISwapic.Expired.selector);
        swapic.run(o);

        vm.warp(o.deadline);
        swapic.pause();
        vm.prank(user);
        vm.expectRevert(ISwapic.IsPaused.selector);
        swapic.run(o);
        swapic.unpause();

        vm.prank(user);
        swapic.run(o);
        vm.prank(user);
        vm.expectRevert(ISwapic.Used.selector);
        swapic.run(o);
    }

    function test_run_with_permit2() public {
        vm.prank(user);
        usdc.approve(PERMIT2, type(uint256).max);
        Order memory o = _usdcOrder();
        bytes memory sig = _permit2Sig(o, 1, block.timestamp + 60);
        vm.prank(user);
        swapic.runWithPermit2(o, Permit2Auth(1, block.timestamp + 60, sig));
        (uint256 burned,,,,,,,) = messenger.last();
        assertEq(burned, 100e6 - FEE);
        _assertEmpty();
    }

    function test_3009_door() public {
        Order memory o = _usdcOrder();
        GaslessAuth memory a = _auth3009(o);
        vm.prank(canister);
        swapic.runFor(o, a);
        assertEq(usdc.balanceOf(canister), FEE);
        (uint256 burned,,,,,,,) = messenger.last();
        assertEq(burned, 100e6 - FEE);
        assertEq(usdc.balanceOf(user), 900e6);
        _assertEmpty();
    }

    function test_2612_door_needs_the_payers_acceptance() public {
        Order memory o = _usdcOrder();
        GaslessAuth memory a = _auth2612(o, strangerKey);
        vm.prank(canister);
        vm.expectRevert(ISwapic.NotAccepted.selector);
        swapic.runFor(o, a);

        a = _auth2612(o, userKey);
        vm.prank(canister);
        swapic.runFor(o, a);
        assertEq(usdc.balanceOf(user), 900e6);
        _assertEmpty();
    }

    function test_2612_door_accepts_an_erc1271_payer() public {
        Wallet1271 wallet = new Wallet1271(user);
        usdc.transfer(address(wallet), 100e6);
        wallet.approve(address(usdc), address(swapic));
        Order memory o = _usdcOrder();
        o.payer = address(wallet);
        GaslessAuth memory a;
        a.door = Door.Permit2612;
        a.acceptance = _sig(strangerKey, swapic.acceptanceDigest(o));
        vm.prank(canister);
        vm.expectRevert(ISwapic.NotAccepted.selector);
        swapic.runFor(o, a);

        a.acceptance = _sig(userKey, swapic.acceptanceDigest(o));
        vm.prank(canister);
        swapic.runFor(o, a);
        assertEq(usdc.balanceOf(address(wallet)), 0);
        _assertEmpty();
    }

    function test_permit2_door() public {
        vm.prank(user);
        usdc.approve(PERMIT2, type(uint256).max);
        Order memory o = _usdcOrder();
        GaslessAuth memory a = _authPermit2(o);
        vm.prank(canister);
        swapic.runFor(o, a);
        assertEq(usdc.balanceOf(user), 900e6);
        _assertEmpty();
    }

    /// A token whose fallback takes any selector turns `receiveWithAuthorization`
    /// into a silent no-op that checks no signature; Swapic's own stray balance
    /// must not pay for the order.
    function test_3009_door_refuses_a_no_op_receive() public {
        FallbackToken t = new FallbackToken();
        t.transfer(address(swapic), 100e6);
        Order memory o = _usdcOrder();
        o.tokenIn = address(t);
        GaslessAuth memory a;
        a.door = Door.Auth3009;
        a.validBefore = block.timestamp + 1 hours;
        a.signature = new bytes(65);
        vm.prank(canister);
        vm.expectRevert(ISwapic.NotReceived.selector);
        swapic.runFor(o, a);
        assertEq(t.balanceOf(address(swapic)), 100e6, "the stray balance stays put");
    }

    function test_every_door_needs_exactly_amount_in() public {
        usdc.setFeeBps(10);
        Order memory o = _usdcOrder();
        vm.prank(user);
        vm.expectRevert(ISwapic.NotReceived.selector);
        swapic.run(o);

        GaslessAuth memory a = _auth2612(o, userKey);
        vm.prank(canister);
        vm.expectRevert(ISwapic.NotReceived.selector);
        swapic.runFor(o, a);

        a = _auth3009(o);
        vm.prank(canister);
        vm.expectRevert(ISwapic.NotReceived.selector);
        swapic.runFor(o, a);

        vm.prank(user);
        usdc.approve(PERMIT2, type(uint256).max);
        a = _authPermit2(o);
        vm.prank(canister);
        vm.expectRevert(ISwapic.NotReceived.selector);
        swapic.runFor(o, a);
    }

    function test_order_started_comes_after_the_receipt() public {
        Order memory o = _usdcOrder();
        vm.recordLogs();
        vm.prank(user);
        swapic.run(o);
        Vm.Log[] memory logs = vm.getRecordedLogs();
        uint256 started;
        uint256 pulled;
        for (uint256 i; i < logs.length; ++i) {
            if (logs[i].topics[0] == ISwapic.OrderStarted.selector) started = i;
            if (logs[i].emitter == address(usdc) && logs[i].topics[0] == keccak256("Transfer(address,address,uint256)") && pulled == 0) {
                pulled = i + 1;
            }
        }
        assertGt(started + 1, pulled, "OrderStarted after the pull");
    }

    /// One Permit2 address: the door Swapic pulls through is the target the
    /// Runner refuses.
    function test_one_permit2_for_the_door_and_the_ban() public {
        assertEq(PERMIT2, 0x000000000022D473030F116dDEE9F6B43aC78BA3);
        vm.prank(user);
        usdc.approve(PERMIT2, type(uint256).max);
        Order memory o = _usdcOrder();
        bytes memory sig = _permit2Sig(o, 1, block.timestamp + 60);
        vm.prank(user);
        swapic.runWithPermit2(o, Permit2Auth(1, block.timestamp + 60, sig));

        Order memory b = _usdcOrder();
        b.ref = keccak256("permit2 target");
        b.steps[0] = Step(PERMIT2, hex"00000000", address(0), 0, new Write[](0));
        vm.prank(user);
        vm.expectRevert(IRunner.BadStep.selector);
        swapic.run(b);
    }

    // ---- Review Focus 5 ----

    function _variants(Order memory o) internal view returns (Order[] memory vs) {
        vs = new Order[](5);
        for (uint256 i; i < 5; ++i) {
            vs[i] = _usdcOrder();
        }
        vs[0].steps[0] = Step(address(usdc), abi.encodeCall(IERC20.transfer, (canister, FEE + 1)), address(0), 0, new Write[](0));
        vs[1].steps[1].writes[0].minus = 1;
        vs[2].payout = Payout(address(usdc), stranger, 1);
        vs[3].amountIn = o.amountIn + 1;
        vs[4].steps[1].min = 1;
    }

    function test_gasless_changed_plan_rejected() public {
        vm.prank(user);
        usdc.approve(PERMIT2, type(uint256).max);
        Order memory o = _usdcOrder();
        Order[] memory vs = _variants(o);

        GaslessAuth memory a3009 = _auth3009(o);
        GaslessAuth memory a2612 = _auth2612(o, userKey);
        GaslessAuth memory aP2 = _authPermit2(o);
        vm.startPrank(canister);
        for (uint256 i; i < vs.length; ++i) {
            assertTrue(swapic.orderId(vs[i]) != swapic.orderId(o));
            vm.expectRevert("FiatTokenV2: invalid signature");
            swapic.runFor(vs[i], a3009);
            vm.expectRevert(ISwapic.NotAccepted.selector);
            swapic.runFor(vs[i], a2612);
            vm.expectRevert("InvalidSigner");
            swapic.runFor(vs[i], aP2);
        }

        swapic.runFor(o, a3009);
        vm.expectRevert(ISwapic.Used.selector);
        swapic.runFor(o, a3009);
        vm.expectRevert(ISwapic.Used.selector);
        swapic.runFor(o, a2612);
        vm.expectRevert(ISwapic.Used.selector);
        swapic.runFor(o, aP2);
        vm.stopPrank();

        // a fresh order for each of the other two doors, each also once only
        o.ref = keccak256("second");
        a2612 = _auth2612(o, userKey);
        vm.prank(canister);
        swapic.runFor(o, a2612);
        vm.prank(canister);
        vm.expectRevert(ISwapic.Used.selector);
        swapic.runFor(o, a2612);

        o.ref = keccak256("third");
        aP2 = _authPermit2(o);
        vm.prank(canister);
        swapic.runFor(o, aP2);
        vm.prank(canister);
        vm.expectRevert(ISwapic.Used.selector);
        swapic.runFor(o, aP2);
        assertEq(usdc.balanceOf(user), 700e6, "exactly three orders paid");
        _assertEmpty();
    }

    // ---- administration ----

    function test_administration() public {
        vm.prank(stranger);
        vm.expectRevert(ISwapic.NotGuardianOrOwner.selector);
        swapic.pause();
        vm.prank(guardian);
        swapic.pause();
        assertTrue(swapic.paused());
        vm.prank(guardian);
        vm.expectRevert(ISwapic.NotOwner.selector);
        swapic.unpause();
        swapic.unpause();

        vm.expectRevert(ISwapic.BadParams.selector);
        swapic.setCanister(address(0));
        swapic.setCanister(address(0x0E));
        assertEq(swapic.canister(), address(0x0E));
        swapic.setGuardian(address(0x6B));
        assertEq(swapic.guardian(), address(0x6B));

        swapic.transferOwnership(stranger);
        vm.prank(user);
        vm.expectRevert(ISwapic.NotOwner.selector);
        swapic.acceptOwnership();
        vm.prank(stranger);
        swapic.acceptOwnership();
        assertEq(swapic.owner(), stranger);
        assertEq(swapic.pendingOwner(), address(0));
        vm.expectRevert(ISwapic.NotOwner.selector);
        swapic.setCanister(address(1));
    }

    function test_constructor_refuses_zero_owner_or_canister() public {
        vm.expectRevert(ISwapic.BadParams.selector);
        new Swapic(
            address(usdc), address(transmitter), CctpPlan.TOKEN_MESSENGER, address(wrapped), address(0), address(0), guardian, canister
        );
        vm.expectRevert(ISwapic.BadParams.selector);
        new Swapic(
            address(usdc), address(transmitter), CctpPlan.TOKEN_MESSENGER, address(wrapped), address(0), address(this), guardian, address(0)
        );
    }
}

/// Review Focus 4: a step or recipe target calls back into Swapic.run,
/// Runner.fill, Runner.execute or CctpReceiver.completeCctp. The target
/// records the refusal's selector and carries on, so the record survives.
contract ReentryTest is SwapicBase {
    Reenterer re;
    address executor = address(0xEC0E);
    uint256 nonce;

    function setUp() public override {
        super.setUp();
        re = new Reenterer();
        usdc.transfer(address(re), 100e6);
        weth.transfer(address(re), 1e18);
        re.approve(address(usdc), address(swapic));
        usdc.transfer(executor, 1000e6);
    }

    function _innerOrder() internal view returns (Order memory o) {
        o = _usdcOrder();
        o.payer = address(re);
        o.ref = keccak256("inner");
        o.amountIn = 10e6;
        o.steps = new Step[](1);
        o.steps[0] = _burnStep(0);
    }

    function _message() internal returns (bytes memory) {
        return CctpPlan.message(
            6, 3, bytes32(++nonce), address(receiver), address(receiver), address(usdc), 10e6, 0, _envelope(address(re))
        );
    }

    function _swapRecipe() internal view returns (Recipe memory) {
        return Recipe(address(dex), abi.encodeCall(DexMock.swap, (address(usdc), address(weth), 10e6, 1)), address(0));
    }

    /// Every way back in, as calldata for the reenterer.
    function _targets() internal returns (address[] memory ts, bytes[] memory ds) {
        ts = new address[](4);
        ds = new bytes[](4);
        ts[0] = address(swapic);
        ds[0] = abi.encodeCall(Swapic.run, (_innerOrder()));
        ts[1] = address(runner);
        ds[1] = abi.encodeCall(Runner.fill, (address(usdc), 1, _envelope(address(re)), _swapRecipe()));
        ts[2] = address(runner);
        ds[2] = abi.encodeCall(Runner.execute, (_innerOrder(), bytes32(0), 0));
        ts[3] = address(receiver);
        ds[3] = abi.encodeCall(CctpReceiver.completeCctp, (_message(), "", _swapRecipe()));
    }

    function test_reentry_from_step_reverts() public {
        (address[] memory ts, bytes[] memory ds) = _targets();
        for (uint256 i; i < ts.length; ++i) {
            re.arm(ts[i], ds[i]);
            Order memory o = _usdcOrder();
            o.ref = bytes32(i);
            o.steps = new Step[](2);
            o.steps[0] = Step(address(re), abi.encodeCall(Reenterer.poke, ()), address(0), 0, new Write[](0));
            o.steps[1] = _burnStep(0);
            vm.prank(user);
            swapic.run(o);
            assertEq(re.caught(), ISwapic.Reentrancy.selector, "refused by a lock");
        }
        assertEq(usdc.balanceOf(address(re)), 100e6, "no inner order ran");
        _assertEmpty();
    }

    function test_reentry_from_recipe_reverts() public {
        (address[] memory ts, bytes[] memory ds) = _targets();
        Recipe memory r = Recipe(
            address(re), abi.encodeCall(Reenterer.swapAndPoke, (address(usdc), address(weth), 100e6, 6e16)), address(0)
        );
        for (uint256 i; i < ts.length; ++i) {
            re.arm(ts[i], ds[i]);
            vm.startPrank(executor);
            usdc.approve(address(runner), 100e6);
            runner.fill(address(usdc), 100e6, _envelope(user), r);
            vm.stopPrank();
            assertEq(re.caught(), IRunner.Reentrancy.selector, "refused by a lock");
        }
        assertEq(weth.balanceOf(user), 10e18 + 4 * 6e16, "each arrival still delivered whole");
        assertEq(usdc.balanceOf(address(re)), 100e6 + 4 * 100e6, "no inner order ran");

        // and from inside completeCctp, the receiver's own lock
        re.arm(address(receiver), abi.encodeCall(CctpReceiver.completeCctp, (_message(), "", _swapRecipe())));
        bytes memory m = CctpPlan.message(
            6, 3, bytes32(++nonce), address(receiver), address(receiver), address(usdc), 100e6, 0, _envelope(user)
        );
        vm.prank(canister);
        receiver.completeCctp(m, "", r);
        assertEq(re.caught(), ICctpReceiver.Reentrancy.selector);
        assertEq(weth.balanceOf(user), 10e18 + 5 * 6e16);
        _assertEmpty();
    }
}

/// Review fix: a transfer hook that runs during a door's pull (the payer's
/// tokens are already in the Runner) and calls the still-open `Runner.fill`
/// is refused, and the order runs on the full amount.
contract FillHook is ITransferHook {
    bytes4 public caught;
    bool public done;
    address public payer;
    address public target;
    bytes public data;

    function arm(address payer_, address target_, bytes calldata data_) external {
        payer = payer_;
        target = target_;
        data = data_;
    }

    function onTransfer(address from, address, uint256) external {
        if (done || from != payer) return;
        done = true;
        (bool ok, bytes memory ret) = target.call(data);
        caught = ok ? bytes4(0xffffffff) : bytes4(ret);
    }

    function approve(address token, address spender) external {
        IERC20(token).approve(spender, type(uint256).max);
    }
}

contract HookedPullTest is SwapicBase {
    function test_no_fill_during_a_pull() public {
        HookedToken hooked = new HookedToken();
        hooked.transfer(user, 100e6);
        vm.prank(user);
        hooked.approve(address(swapic), type(uint256).max);
        FillHook h = new FillHook();
        usdc.transfer(address(h), 10e6);
        h.approve(address(usdc), address(runner));
        bytes memory env = Envelope.encode(
            Envelope.Data(3, keccak256("hook"), Action.Swap, uint64(block.timestamp + 1 hours), address(h), address(weth), 1, 0, 0, 0)
        );
        Recipe memory r =
            Recipe(address(dex), abi.encodeCall(DexMock.swap, (address(usdc), address(weth), 1e6, 1)), address(0));
        h.arm(user, address(runner), abi.encodeCall(Runner.fill, (address(usdc), 1e6, env, r)));
        hooked.setHook(address(h));

        Order memory o = _usdcOrder();
        o.tokenIn = address(hooked);
        o.steps = new Step[](2);
        o.steps[0] = Step(
            address(dex),
            abi.encodeCall(DexMock.swap, (address(hooked), address(usdc), 100e6, 100e6)),
            address(hooked),
            100e6,
            new Write[](0)
        );
        o.steps[1] = _burnStep(100e6);
        vm.prank(user);
        swapic.run(o);
        assertTrue(h.done(), "the hook ran during the pull");
        assertEq(h.caught(), IRunner.Reentrancy.selector, "fill refused while the door runs");
        (uint256 burned,,,,,,,) = messenger.last();
        assertEq(burned, 100e6, "the order ran on the full amount");
        assertEq(usdc.balanceOf(address(h)), 10e6, "the hook's fill did not run");
        _assertEmpty();
    }
}

/// The goldens of OrderHash.t.sol from a deployed Swapic at the golden
/// address and chain.
contract SwapicGoldenTest is GoldenOrder {
    function test_a_deployed_swapic_reports_the_goldens() public {
        Swapic s = new Swapic(address(0x5757), address(0), address(0), address(0), address(0), address(this), address(0), address(0xCA));
        vm.etch(SWAPIC, address(s).code);
        vm.chainId(CHAIN);
        Swapic golden = Swapic(SWAPIC);
        assertEq(golden.orderId(_golden()), 0x2f21eee628b331d82ca45f067ec51c73de0533289d22c22328cf4ebd43e255bc);
        assertEq(
            golden.acceptanceDigest(_golden()), 0x5058c6a5e030b5beb7c0a929627f189d69cbf98e8801d7e5bb868f4805937d0d
        );
        (bytes1 fields, string memory name, string memory version, uint256 chainId, address verifying,,) =
            golden.eip712Domain();
        assertEq(fields, hex"0f");
        assertEq(name, "Swapic");
        assertEq(version, "3");
        assertEq(chainId, CHAIN);
        assertEq(verifying, SWAPIC);
    }
}
