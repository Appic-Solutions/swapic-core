// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "./ForkBase.sol";
import {IScaler} from "../../src/interfaces/IScaler.sol";

/// Arbitrum: `fill` with a real KyberSwap USDC to WETH recipe built for 100
/// USDC, rescaled by the real InputScalingHelper to what actually arrived.
/// The helper itself refuses a new amount more than 5% away from the recipe's
/// (measured: 95 and 105 USDC rescale, 94 and 106 do not, on this recipe and
/// on a camelot-v3 one); the Runner then refunds all of it.
contract ForkFillTest is ForkBase {
    bytes32 constant DELIVERED = keccak256("Delivered(bytes32,address,address,uint256,uint256)");
    address executor = address(0xEC0E);
    bytes32 constant REF = keccak256("fork fill");
    uint256 forkBlock;

    function _setUp(string memory name) internal returns (bool ok, string memory j) {
        j = _fixture(name);
        forkBlock = vm.parseJsonUint(j, ".block");
        ok = _fork("arbitrum", "ARBITRUM_RPC_URL", forkBlock);
        if (ok) _deploy(USDC_ARB, WETH_ARB);
    }

    function _fill(uint256 amount, uint256 minOut, Recipe memory r, string memory label) internal returns (uint256 out) {
        deal(USDC_ARB, executor, amount);
        uint256 wethBefore = IERC20(WETH_ARB).balanceOf(payer);
        vm.prank(executor);
        IERC20(USDC_ARB).approve(RUNNER, amount);
        bytes memory cd =
            abi.encodeCall(Runner.fill, (USDC_ARB, amount, _envelope(REF, Action.Swap, payer, WETH_ARB, minOut), r));
        vm.prank(executor);
        _call(label, forkBlock, RUNNER, cd);
        out = IERC20(WETH_ARB).balanceOf(payer) - wethBefore;
        assertEq(IERC20(USDC_ARB).balanceOf(executor), 0, "pulled exactly amount");
        assertEq(IERC20(USDC_ARB).allowance(RUNNER, r.target), 0);
        _assertRunnerEmpty(USDC_ARB, WETH_ARB);
    }

    function _scaled(uint256 amount) internal {
        (bool ok, string memory j) = _setUp("kyber_arb_usdc_weth_100");
        if (!ok) return;
        uint256 quoted = vm.parseJsonUint(j, ".amountOut");
        uint256 minOut = quoted * amount / 100e6 * 97 / 100;
        Recipe memory r = Recipe(vm.parseJsonAddress(j, ".routerAddress"), vm.parseJsonBytes(j, ".data"), SCALER);
        vm.recordLogs();
        uint256 out = _fill(amount, minOut, r, string.concat("fill: KyberSwap, rescaled to ", vm.toString(amount)));
        Vm.Log[] memory logs = vm.getRecordedLogs();
        bool seen;
        for (uint256 i; i < logs.length; ++i) {
            if (logs[i].emitter == RUNNER && logs[i].topics[0] == DELIVERED) {
                (, uint256 amountIn, uint256 amountOut) = abi.decode(logs[i].data, (address, uint256, uint256));
                assertEq(amountIn, amount, "all of what arrived was spent");
                assertEq(amountOut, out);
                seen = true;
            }
        }
        assertTrue(seen, "delivered, not refunded");
        assertGe(out, minOut);
        assertEq(IERC20(USDC_ARB).balanceOf(payer), 0, "no USDC beside the WETH");
        console2.log("amount", amount);
        console2.log("weth out", out);
    }

    function test_rescaled_down() public {
        _scaled(96e6);
    }

    function test_rescaled_up() public {
        _scaled(104_500_000);
    }

    function _refusedByScaler(uint256 amount) internal {
        (bool ok, string memory j) = _setUp("kyber_arb_usdc_weth_100");
        if (!ok) return;
        Recipe memory r = Recipe(vm.parseJsonAddress(j, ".routerAddress"), vm.parseJsonBytes(j, ".data"), SCALER);
        (bool scaled,) = IScaler(SCALER).getScaledInputData(r.data, amount);
        assertFalse(scaled, "the real helper refuses beyond 5%");
        uint256 out = _fill(amount, 1, r, "fill: scaler refuses, refund all");
        assertEq(out, 0, "no WETH");
        assertEq(IERC20(USDC_ARB).balanceOf(payer), amount, "all of the USDC");
    }

    function test_scaler_refusal_far_below_refunds_all() public {
        _refusedByScaler(80e6);
    }

    function test_scaler_refusal_far_above_refunds_all() public {
        _refusedByScaler(125e6);
    }

    function test_exact_amount_without_scaler() public {
        (bool ok, string memory j) = _setUp("kyber_arb_usdc_weth_100");
        if (!ok) return;
        Recipe memory r = Recipe(vm.parseJsonAddress(j, ".routerAddress"), vm.parseJsonBytes(j, ".data"), address(0));
        uint256 out = _fill(100e6, vm.parseJsonUint(j, ".amountOut") * 97 / 100, r, "fill: KyberSwap 100 USDC, no scaler");
        assertGt(out, 0);
        assertEq(IERC20(USDC_ARB).balanceOf(payer), 0);
    }

    function test_rescaled_odd_amount() public {
        _scaled(99_941_269);
    }

    function test_recipe_for_twice_the_amount_refunds_all() public {
        (bool ok, string memory j) = _setUp("kyber_arb_usdc_weth_200");
        if (!ok) return;
        Recipe memory r = Recipe(vm.parseJsonAddress(j, ".routerAddress"), vm.parseJsonBytes(j, ".data"), address(0));
        uint256 out = _fill(100e6, 1, r, "fill: router reverts, refund all");
        assertEq(out, 0, "no WETH");
        assertEq(IERC20(USDC_ARB).balanceOf(payer), 100e6, "all of the USDC");
    }

    /// A real KyberSwap route starved at every gas limit up to its need: a
    /// caller can make it revert, never turn it into a refund.
    function test_starving_a_real_route_never_refunds() public {
        (bool ok, string memory j) = _setUp("kyber_arb_usdc_weth_100");
        if (!ok) return;
        Recipe memory r = Recipe(vm.parseJsonAddress(j, ".routerAddress"), vm.parseJsonBytes(j, ".data"), address(0));
        bytes memory env = _envelope(REF, Action.Swap, payer, WETH_ARB, 1);
        deal(USDC_ARB, executor, 100e6);
        vm.prank(executor);
        IERC20(USDC_ARB).approve(RUNNER, type(uint256).max);
        uint256[3] memory seen;
        uint256 firstDelivered;
        for (uint256 gas = 100_000; gas < 1_000_000; gas += 5_000) {
            uint256 snap = vm.snapshotState();
            vm.prank(executor);
            try runner.fill{gas: gas}(USDC_ARB, 100e6, env, r) {
                if (IERC20(WETH_ARB).balanceOf(payer) > 0) {
                    ++seen[0];
                    if (firstDelivered == 0) firstDelivered = gas;
                } else {
                    ++seen[1];
                }
            } catch {
                ++seen[2];
            }
            vm.revertToState(snap);
        }
        console2.log("delivered", seen[0], "from gas", firstDelivered);
        console2.log("refunded", seen[1]);
        console2.log("reverted", seen[2]);
        assertEq(seen[1], 0, "never refunded");
        assertGt(seen[0], 0);
    }
}
