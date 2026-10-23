// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "./ForkBase.sol";
import "../helpers/EcoPlan.sol";
import {IERC20Permit} from "@openzeppelin/contracts/token/ERC20/extensions/IERC20Permit.sol";

/// Base: the gas of two canister transactions abi.md section 10 lacked, each
/// `runFor` Auth3009 on real USDC with the canister fee step:
///   - same chain: a real KyberSwap USDC to WETH swap paying the Runner, then
///     a WETH payout to the user;
///   - Eco: a `publishAndFund` Swap-route step on the real Portal 2.12.
contract ForkGaslessOrdersTest is ForkBase {
    bytes32 constant RECEIVE_TYPEHASH = 0xd099cc98ef71107a616c4f0f941f04c322d8e254fe26b3c6668db87aae413de8;
    address constant PORTAL = 0xEC000769A73b70e16f361a442292500b3BCf4A85;
    uint256 constant FEE = 50_000;

    function _feeStep() internal view returns (Step memory) {
        return Step(USDC_BASE, abi.encodeCall(IERC20.transfer, (canister, FEE)), address(0), 0, new Write[](0));
    }

    function _auth3009(Order memory o) internal view returns (GaslessAuth memory a) {
        a.door = Door.Auth3009;
        a.validBefore = block.timestamp + 1 hours;
        bytes32 structHash =
            keccak256(abi.encode(RECEIVE_TYPEHASH, o.payer, SWAPIC, o.amountIn, 0, a.validBefore, swapic.orderId(o)));
        a.signature = _sig(
            PAYER_KEY, keccak256(abi.encodePacked("\x19\x01", IERC20Permit(USDC_BASE).DOMAIN_SEPARATOR(), structHash))
        );
    }

    function test_gasless_same_chain_swap_and_payout() public {
        string memory j = _fixture("kyber_base_usdc_weth");
        uint256 blk = vm.parseJsonUint(j, ".block");
        if (!_fork("base", "BASE_RPC_URL", blk)) return;
        _deploy(USDC_BASE, WETH_BASE);
        uint256 swapIn = vm.parseJsonUint(j, ".amountIn");
        uint256 minOut = vm.parseJsonUint(j, ".amountOut") * 97 / 100;
        deal(USDC_BASE, payer, swapIn + FEE);

        Order memory o;
        o.ref = keccak256("gasless same chain");
        o.payer = payer;
        o.tokenIn = USDC_BASE;
        o.amountIn = swapIn + FEE;
        o.deadline = block.timestamp + 60;
        o.steps = new Step[](2);
        o.steps[0] = _feeStep();
        o.steps[1] =
            Step(vm.parseJsonAddress(j, ".routerAddress"), vm.parseJsonBytes(j, ".data"), USDC_BASE, swapIn, new Write[](0));
        o.payout = Payout(WETH_BASE, payer, minOut);

        bytes memory cd = abi.encodeCall(Swapic.runFor, (o, _auth3009(o)));
        vm.prank(canister);
        _call("runFor 3009: canister fee + KyberSwap USDC to WETH + WETH payout", blk, SWAPIC, cd);

        assertGe(IERC20(WETH_BASE).balanceOf(payer), minOut, "the user got the WETH");
        assertEq(IERC20(USDC_BASE).balanceOf(payer), 0);
        assertEq(IERC20(USDC_BASE).balanceOf(canister), FEE, "the fee step paid the canister");
        _assertRunnerEmpty(USDC_BASE, WETH_BASE);
    }

    function test_gasless_eco_publish() public {
        string memory j = _fixture("eco_base_arb_standin");
        uint256 blk = vm.parseJsonUint(j, ".block");
        if (!_fork("base", "BASE_RPC_URL", blk)) return;
        _deploy(USDC_BASE, WETH_BASE);

        EcoPlan.Terms memory t;
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

        string memory k = _fixture("kyber_arb_usdc_weth_100");
        Recipe memory recipe =
            Recipe(vm.parseJsonAddress(k, ".routerAddress"), vm.parseJsonBytes(k, ".data"), SCALER);
        bytes memory env = _envelope(keccak256("gasless eco"), Action.Swap, payer, WETH_ARB, 2e15);
        uint256 x = 5_013_742;
        deal(USDC_BASE, payer, x + FEE);

        Order memory o;
        o.ref = keccak256("gasless eco");
        o.payer = payer;
        o.tokenIn = USDC_BASE;
        o.amountIn = x + FEE;
        o.deadline = block.timestamp + 60;
        o.steps = new Step[](2);
        o.steps[0] = _feeStep();
        o.steps[1] = EcoPlan.swapStep(t, PORTAL, RUNNER, env, recipe, 5e6);

        bytes memory cd = abi.encodeCall(Swapic.runFor, (o, _auth3009(o)));
        vm.prank(canister);
        _call("runFor 3009: canister fee + Eco publishAndFund, Swap route", blk, SWAPIC, cd);

        bytes memory routeX = EcoPlan.route(t, EcoPlan.swapCalls(t, RUNNER, x - t.fee, env, recipe), x - t.fee);
        address vault = IEcoPortal(PORTAL).intentVaultAddress(t.destination, routeX, EcoPlan.reward(t, x));
        assertEq(IERC20(USDC_BASE).balanceOf(vault), x, "the reward vault holds exactly X");
        assertEq(IERC20(USDC_BASE).balanceOf(canister), FEE, "the fee step paid the canister");
        _assertRunnerEmpty(USDC_BASE, WETH_BASE);
    }
}
