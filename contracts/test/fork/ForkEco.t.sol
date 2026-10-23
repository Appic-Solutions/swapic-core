// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "./ForkBase.sol";
import "../helpers/EcoPlan.sol";

/// Base: a USDC order whose one step is `publishAndFund` on the real Eco
/// Portal 2.12, built by EcoPlan with the amounts written in at run time, at
/// an amount Eco never quoted. Terms (salt, deadlines, prover, fee) from a
/// real Eco quote of the same shape (fixture `eco_base_arb_standin`).
contract ForkEcoTest is ForkBase {
    address constant PORTAL = 0xEC000769A73b70e16f361a442292500b3BCf4A85;
    bytes32 constant INTENT_PUBLISHED = 0x43974895be1bcec7344337863fa7de24a0d1c315c0a994f663fe0ee220ddc8e4;
    uint256 constant X = 5_013_742;

    string j;
    EcoPlan.Terms t;

    function setUp() public {
        j = _fixture("eco_base_arb_standin");
        if (!_fork("base", "BASE_RPC_URL", vm.parseJsonUint(j, ".block"))) return;
        assertEq(IEcoPortal(PORTAL).version(), "2.12.0");
        assertEq(vm.parseJsonAddress(j, ".portal"), PORTAL, "the Portal Eco's quote names");
        _deploy(USDC_BASE, WETH_BASE);
        deal(USDC_BASE, payer, 20e6);
        vm.prank(payer);
        IERC20(USDC_BASE).approve(SWAPIC, type(uint256).max);

        t.destination = 42161;
        t.portal = PORTAL;
        t.dstToken = USDC_ARB;
        t.srcToken = USDC_BASE;
        t.salt = vm.parseJsonBytes32(j, ".salt");
        t.routeDeadline = uint64(vm.parseJsonUint(j, ".routeDeadline"));
        t.rewardDeadline = uint64(vm.parseJsonUint(j, ".rewardDeadline"));
        t.creator = payer;
        t.prover = vm.parseJsonAddress(j, ".prover");
        t.fee = vm.parseJsonUint(j, ".fee");
    }

    function _order(Step memory s) internal view returns (Order memory o) {
        o.ref = keccak256("fork eco");
        o.payer = payer;
        o.tokenIn = USDC_BASE;
        o.amountIn = X;
        o.deadline = block.timestamp + 60;
        o.steps = new Step[](1);
        o.steps[0] = s;
    }

    function _publishAndCheck(Order memory o, bytes memory routeX, string memory label) internal {
        IEcoPortal.Reward memory rewardX = EcoPlan.reward(t, X);
        bytes32 expected = EcoPlan.intentHash(t, routeX, rewardX);

        bytes memory cd = abi.encodeCall(Swapic.run, (o));
        uint256 blk = vm.parseJsonUint(j, ".block");
        vm.recordLogs();
        vm.prank(payer);
        _call(label, blk, SWAPIC, cd);
        Vm.Log[] memory logs = vm.getRecordedLogs();
        bool seen;
        for (uint256 i; i < logs.length; ++i) {
            if (logs[i].emitter == PORTAL && logs[i].topics[0] == INTENT_PUBLISHED) {
                assertEq(logs[i].topics[1], expected, "the Portal's intent hash is EcoPlan's at the real amount");
                assertEq(logs[i].topics[2], bytes32(uint256(uint160(payer))), "creator is the payer");
                (, bytes memory publishedRoute) = abi.decode(logs[i].data, (uint64, bytes));
                assertEq(publishedRoute, routeX, "the published route carries X - fee everywhere");
                seen = true;
            }
        }
        assertTrue(seen, "published on the real Portal");
        (bytes32 h,,) = IEcoPortal(PORTAL).getIntentHash(t.destination, routeX, rewardX);
        assertEq(h, expected);
        address vault = IEcoPortal(PORTAL).intentVaultAddress(t.destination, routeX, rewardX);
        assertEq(IERC20(USDC_BASE).balanceOf(vault), X, "the reward vault holds exactly X");
        assertTrue(IEcoPortal(PORTAL).isIntentFunded(t.destination, routeX, rewardX));
        _assertRunnerEmpty(USDC_BASE, WETH_BASE);
        assertEq(IERC20(USDC_BASE).allowance(RUNNER, PORTAL), 0);
        console2.log("block", block.number, "intent");
        console2.logBytes32(expected);
    }

    function test_publish_swap_route_with_written_amounts() public {
        string memory k = _fixture("kyber_arb_usdc_weth_100");
        Recipe memory recipe =
            Recipe(vm.parseJsonAddress(k, ".routerAddress"), vm.parseJsonBytes(k, ".data"), SCALER);
        bytes memory env = _envelope(keccak256("fork eco"), Action.Swap, payer, WETH_ARB, 2e15);
        Step memory s = EcoPlan.swapStep(t, PORTAL, RUNNER, env, recipe, 5e6);
        assertEq(s.writes.length, 4);
        bytes memory routeX = EcoPlan.route(t, EcoPlan.swapCalls(t, RUNNER, X - t.fee, env, recipe), X - t.fee);
        _publishAndCheck(_order(s), routeX, "run: one Eco publishAndFund step, Swap route (KyberSwap recipe inside)");
    }

    function test_publish_deliver_route_with_written_amounts() public {
        Step memory s = EcoPlan.deliverStep(t, PORTAL, payer, 5e6);
        bytes memory routeX = EcoPlan.route(t, EcoPlan.deliverCalls(t, payer, X - t.fee), X - t.fee);
        _publishAndCheck(_order(s), routeX, "run: one Eco publishAndFund step, Deliver route");
    }
}
