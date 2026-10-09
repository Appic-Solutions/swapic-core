// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "forge-std/Test.sol";
import "../src/SwapicRouter.sol";
import "../src/SwapicTypes.sol";
import {HookData} from "../src/libraries/HookData.sol";
import "./mocks/RailMocks.sol";
import "./mocks/TestToken.sol";

contract SwapicRouterTest is Test {
    SwapicRouter router;
    TestToken usdc;
    TestToken weth;
    CctpMessengerMock messenger;
    EcoPortalV2Mock portal;
    DexMock dex;

    address user = address(0xA11CE);
    address stranger = address(0xBAD);
    address guardian = address(0x6A);
    address relayer = address(0xCA);
    address executor = address(0xE0E0);
    address dstPortal = 0xEC000064576f9C95a8623Bc0eff3db6d296ea6df;
    address dstUsdc = 0xaf88d065e77c8cC2239327C5EDb3A432268e5831;
    address prover = 0xec004Ab4870c4e177c66949329dCdb503CE41022;
    uint64 constant ARB = 42161;

    function setUp() public {
        vm.warp(1_790_000_000);
        usdc = new TestToken();
        weth = new TestToken();
        messenger = new CctpMessengerMock();
        portal = new EcoPortalV2Mock();
        dex = new DexMock();
        router = new SwapicRouter(address(usdc), address(messenger), address(portal), address(this), guardian, relayer);
        router.setCctpDestination(ARB, 3, bytes32(uint256(uint160(executor))), true);
        router.setEcoDestination(ARB, dstPortal, dstUsdc, executor);
        router.setEcoConfig(prover, 2 hours);
        router.setAllowedTarget(address(dex), true);

        usdc.transfer(address(dex), 1e24);
        usdc.transfer(user, 1e24);
        weth.transfer(user, 1e24);
        vm.startPrank(user);
        usdc.approve(address(router), type(uint256).max);
        weth.approve(address(router), type(uint256).max);
        vm.stopPrank();
    }

    function _base() internal view returns (SwapParams memory p) {
        p.ref = keccak256("ref");
        p.payer = user;
        p.tokenIn = address(usdc);
        p.amountIn = 100e6;
        p.deadline = block.timestamp + 45;
        p.legMinOut = 100e6;
        p.rail = Rail.Cctp;
        p.dstChainId = ARB;
        p.maxFee = 10_000;
        p.minFinality = 1000;
        p.dst.action = Action.Deliver;
        p.dst.recipient = bytes32(uint256(uint160(user)));
        p.dst.minOut = 99e6;
        p.dst.deadline = uint64(block.timestamp + 1 hours);
    }

    function _withLeg(SwapParams memory p, uint256 amountIn, uint256 out) internal view {
        p.tokenIn = address(weth);
        p.amountIn = amountIn;
        p.leg = DexCall(address(dex), abi.encodeCall(DexMock.swap, (address(weth), address(usdc), amountIn, out)));
        p.legMinOut = out;
    }

    function test_cctp_deliver_burns_everything_to_the_user() public {
        SwapParams memory p = _base();
        vm.prank(user);
        bytes32 q = router.swap(p);

        assertEq(q, router.quoteHash(p));
        (uint256 amount, uint32 domain, bytes32 mintRecipient,, bytes32 caller, uint256 maxFee,,) = messenger.last();
        assertEq(amount, 100e6);
        assertEq(domain, 3);
        assertEq(mintRecipient, p.dst.recipient);
        assertEq(caller, bytes32(0), "Deliver: anyone may relay");
        assertEq(maxFee, 10_000);
        HookData.Data memory h = HookData.decode(messenger.lastHookData());
        assertEq(h.ref, q, "the CCTP hook data carries the quote hash");
        assertEq(messenger.lastHookData(), router.hookData(p));
        assertEq(usdc.balanceOf(address(router)), 0, "nothing held");
        assertTrue(router.usedQuote(q));

        vm.prank(user);
        vm.expectRevert(ISwapicRouter.QuoteUsed.selector);
        router.swap(p);
    }

    function test_cctp_swap_runs_the_leg_and_mints_to_the_executor() public {
        SwapParams memory p = _base();
        _withLeg(p, 1e18, 2500e6);
        p.dst.action = Action.Swap;
        p.dst.tokenOut = address(0x7777);
        p.dst.minOut = 1;
        vm.prank(user);
        bytes32 q = router.swap(p);

        (uint256 amount,, bytes32 mintRecipient,, bytes32 caller,,,) = messenger.last();
        assertEq(amount, 2500e6, "the measured leg output, all of it");
        assertEq(mintRecipient, bytes32(uint256(uint160(executor))));
        assertEq(caller, bytes32(uint256(uint160(executor))), "only the executor may receive it");
        HookData.Data memory h = HookData.decode(messenger.lastHookData());
        assertEq(uint8(h.action), uint8(Action.Swap));
        assertEq(h.recipient, user);
        assertEq(h.ref, q);
        assertEq(weth.allowance(address(router), address(dex)), 0, "approval reset");
        assertEq(weth.balanceOf(address(router)), 0);
        assertEq(usdc.balanceOf(address(router)), 0);
    }

    function test_eco_bridges_exactly_the_quote_and_returns_the_excess() public {
        SwapParams memory p = _base();
        _withLeg(p, 1e18, 2500e6);
        p.legMinOut = 2490e6;
        p.rail = Rail.Eco;
        p.eco = EcoTerms({
            salt: keccak256("eco salt"),
            routeDeadline: uint64(block.timestamp + 900),
            rewardDeadline: uint64(block.timestamp + 1530),
            amount: 2490e6,
            amountOut: 2489e6
        });
        p.dst.minOut = 2480e6;
        uint256 before = usdc.balanceOf(user);

        (,,, bytes32 expectedIntent) = router.ecoIntent(p);
        vm.recordLogs();
        vm.prank(user);
        router.swap(p);

        assertEq(usdc.balanceOf(address(portal)), 2490e6, "the Portal took exactly the quoted amount");
        assertEq(usdc.balanceOf(user) - before, 10e6, "the excess went back to the payer");
        assertEq(usdc.balanceOf(address(router)), 0);
        (, bytes memory route,,) = router.ecoIntent(p);
        assertEq(portal.lastRoute(), route, "the router published the route it composed");
        Vm.Log[] memory logs = vm.getRecordedLogs();
        bool seen;
        for (uint256 i = 0; i < logs.length; i++) {
            if (logs[i].topics[0] == ISwapicRouter.SwapStarted.selector) {
                (,,,,, bytes32 ref, bytes32 railRef) =
                    abi.decode(logs[i].data, (uint8, uint256, uint256, bytes32, uint64, bytes32, bytes32));
                assertEq(railRef, expectedIntent, "SwapStarted carries the intent hash");
                assertEq(ref, p.ref);
                seen = true;
            }
        }
        assertTrue(seen);
    }

    function test_a_stranger_cannot_spend_the_payers_approval() public {
        SwapParams memory p = _base();
        p.dst.recipient = bytes32(uint256(uint160(stranger)));
        vm.prank(stranger);
        vm.expectRevert(ISwapicRouter.NotPayer.selector);
        router.swap(p);
    }

    function test_a_leg_that_pays_elsewhere_is_refused() public {
        SwapParams memory p = _base();
        _withLeg(p, 1e18, 2500e6);
        p.leg.data =
            abi.encodeCall(DexMock.swapTo, (address(weth), address(usdc), uint256(1e18), uint256(2500e6), stranger));
        vm.prank(user);
        vm.expectRevert(ISwapicRouter.LegShort.selector);
        router.swap(p);
    }

    function test_the_allowlist_refuses_tokens_and_rails() public {
        vm.expectRevert(ISwapicRouter.BadTarget.selector);
        router.setAllowedTarget(address(weth), true);
        vm.expectRevert(ISwapicRouter.BadTarget.selector);
        router.setAllowedTarget(address(messenger), true);
        vm.expectRevert(ISwapicRouter.BadTarget.selector);
        router.setAllowedTarget(address(portal), true);
    }

    function test_paused_refuses() public {
        vm.prank(guardian);
        router.pause();
        SwapParams memory p = _base();
        vm.prank(user);
        vm.expectRevert(ISwapicRouter.IsPaused.selector);
        router.swap(p);
    }
}
