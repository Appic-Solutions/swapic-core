// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "./helpers/RunnerBase.sol";
import {Envelope} from "../src/libraries/Envelope.sol";

/// A caller of `fill` must not be able to turn a good swap into a refund by
/// starving it of gas: a starved arrival reverts `OutOfGas`, a genuine
/// failure still refunds all.
contract OutOfGasTest is RunnerBase {
    address executor = address(0xEC0E);
    HungryDex hungry;

    function setUp() public override {
        super.setUp();
        hungry = new HungryDex();
        weth.transfer(address(hungry), 1e24);
        usdc.transfer(executor, 1e24);
        vm.prank(executor);
        usdc.approve(address(runner), type(uint256).max);
    }

    function _env(uint256 minOut) internal view returns (bytes memory) {
        return Envelope.encode(
            Envelope.Data(3, keccak256("ref"), Action.Swap, uint64(block.timestamp + 1 hours), user, address(weth), minOut, 0, 0, 0)
        );
    }

    function _recipe(uint256 out, uint256 n, uint256 depth) internal view returns (Recipe memory) {
        return Recipe(
            address(hungry), abi.encodeCall(HungryDex.swap, (address(usdc), address(weth), 100e6, out, n, depth)), address(0)
        );
    }

    /// 0 delivered, 1 refunded, 2 reverted OutOfGas, 3 reverted otherwise
    function _try(uint256 gas, Recipe memory r, uint256 minOut) internal returns (uint256 outcome) {
        uint256 snap = vm.snapshotState();
        vm.prank(executor);
        try runner.fill{gas: gas}(address(usdc), 100e6, _env(minOut), r) {
            outcome = weth.balanceOf(user) > 0 ? 0 : 1;
        } catch (bytes memory err) {
            outcome = bytes4(err) == IRunner.OutOfGas.selector ? 2 : 3;
        }
        vm.revertToState(snap);
    }

    function _sweep(uint256 depth, uint256 n, uint256 from, uint256 to, uint256 step) internal {
        Recipe memory r = _recipe(6e16, n, depth);
        uint256[4] memory seen;
        for (uint256 gas = from; gas < to; gas += step) {
            uint256 o = _try(gas, r, 5e16);
            ++seen[o];
        }
        console2.log("delivered", seen[0]);
        console2.log("refunded", seen[1]);
        console2.log("out of gas", seen[2]);
        console2.log("other revert", seen[3]);
        assertEq(seen[1], 0, "a good swap is never refunded, whatever the gas");
        assertGt(seen[0], 0);
        assertGt(seen[2], 0);
    }

    /// small swaps: the leftover cannot even pay for a refund
    function test_starving_a_cheap_router_never_refunds() public {
        _sweep(0, 2000, 60_000, 1_200_000, 3_000);
        _sweep(1, 2000, 60_000, 1_200_000, 3_000);
    }

    /// about 3.5M gas of routing, starved in its own frame and up to six
    /// calls deeper: the 1/64 each frame keeps adds up
    function test_starving_a_hungry_router_depth_0() public {
        _sweep(0, 30_000, 2_000_000, 5_000_000, 30_000);
    }

    function test_starving_a_hungry_router_depth_1() public {
        _sweep(1, 30_000, 2_000_000, 5_000_000, 30_000);
    }

    function test_starving_a_hungry_router_depth_2() public {
        _sweep(2, 30_000, 2_000_000, 5_000_000, 30_000);
    }

    function test_starving_a_hungry_router_depth_4() public {
        _sweep(4, 30_000, 2_000_000, 5_000_000, 30_000);
    }

    function test_starving_a_hungry_router_depth_6() public {
        _sweep(6, 30_000, 2_000_000, 5_000_000, 30_000);
    }

    function test_a_genuine_short_output_still_refunds_all() public {
        Recipe memory r = _recipe(6e16, 30_000, 3);
        assertEq(_try(30_000_000, r, 7e16), 1, "refunded with plenty of gas");
        vm.prank(executor);
        runner.fill{gas: 30_000_000}(address(usdc), 100e6, _env(7e16), r);
        assertEq(usdc.balanceOf(user), 100e6);
        assertEq(weth.balanceOf(user), 0);
    }

    function test_a_starved_fill_can_be_retried() public {
        Recipe memory r = _recipe(6e16, 30_000, 3);
        vm.prank(executor);
        vm.expectRevert(IRunner.OutOfGas.selector);
        runner.fill{gas: 3_000_000}(address(usdc), 100e6, _env(5e16), r);
        vm.prank(executor);
        runner.fill{gas: 30_000_000}(address(usdc), 100e6, _env(5e16), r);
        assertEq(weth.balanceOf(user), 6e16, "the retry delivers");
        assertEq(usdc.balanceOf(user), 0);
    }

    /// The trade-off of the 1/8 rule: a recipe that fails by burning all the
    /// gas it is given looks starved at any limit, so it reverts `OutOfGas`
    /// and never refunds through `fill` (CCTP: the canister changes recipe or
    /// the message goes to `release`; Eco: the intent refunds on the source).
    function test_a_recipe_that_burns_all_its_gas_reverts_instead_of_refunding() public {
        Recipe memory r = Recipe(address(new BurnsAll()), hex"01", address(0));
        vm.prank(executor);
        vm.expectRevert(IRunner.OutOfGas.selector);
        runner.fill{gas: 30_000_000}(address(usdc), 100e6, _env(5e16), r);
    }

    function _env(Action action, uint256 minOut, uint64 minGas) internal view returns (bytes memory) {
        return Envelope.encode(
            Envelope.Data(3, keccak256("ref"), action, uint64(block.timestamp + 1 hours), user, address(weth), minOut, 0, 0, minGas)
        );
    }

    /// The signed minimum: below it the arrival reverts before the action.
    function test_signed_min_gas_swap() public {
        Recipe memory r = _recipe(6e16, 30_000, 3);
        vm.prank(executor);
        vm.expectRevert(IRunner.OutOfGas.selector);
        runner.fill{gas: 5_000_000}(address(usdc), 100e6, _env(Action.Swap, 5e16, 6_000_000), r);
        vm.prank(executor);
        runner.fill{gas: 30_000_000}(address(usdc), 100e6, _env(Action.Swap, 5e16, 6_000_000), r);
        assertEq(weth.balanceOf(user), 6e16);
    }

    function _ckRunner() internal returns (HungryCkHelper ck) {
        ck = new HungryCkHelper();
        runner = _deployRunner(address(ck));
        vm.prank(executor);
        usdc.approve(address(runner), type(uint256).max);
    }

    function test_signed_min_gas_ck_deposit() public {
        HungryCkHelper ck = _ckRunner();
        Recipe memory none = Recipe(address(0), "", address(0));
        vm.prank(executor);
        vm.expectRevert(IRunner.OutOfGas.selector);
        runner.fill{gas: 5_000_000}(address(usdc), 100e6, _env(Action.CkDeposit, 0, 6_000_000), none);
        vm.prank(executor);
        runner.fill{gas: 30_000_000}(address(usdc), 100e6, _env(Action.CkDeposit, 0, 6_000_000), none);
        assertEq(ck.lastAmount(), 100e6);
        assertEq(usdc.balanceOf(user), 0, "deposited, not refunded");
    }

    /// No signed minimum: a starved ck deposit still never refunds.
    function test_starving_a_ck_deposit_never_refunds() public {
        HungryCkHelper ck = _ckRunner();
        Recipe memory none = Recipe(address(0), "", address(0));
        bytes memory env = _env(Action.CkDeposit, 0, 0);
        uint256[3] memory seen;
        for (uint256 gas = 1_000_000; gas < 5_000_000; gas += 25_000) {
            uint256 snap = vm.snapshotState();
            vm.prank(executor);
            try runner.fill{gas: gas}(address(usdc), 100e6, env, none) {
                seen[usdc.balanceOf(user) > 0 ? 1 : 0]++;
            } catch {
                ++seen[2];
            }
            vm.revertToState(snap);
        }
        assertEq(seen[1], 0, "never refunded");
        assertGt(seen[0], 0, "deposited with enough gas");
        assertGt(seen[2], 0, "reverted when starved");
        assertEq(ck.lastAmount(), 0, "every attempt was rolled back");
    }

    /// Twelve frames below the router the 1/8 rule no longer sees starvation;
    /// the signed minGas closes it when it is at least what the action needs
    /// as given from `fill` (here about 4.4M: each of the 14 frames passes on
    /// only 63/64, so 4M was measured to be too little).
    function test_signed_min_gas_covers_deep_starvation() public {
        Recipe memory r = _recipe(6e16, 30_000, 12);
        uint256[2][2] memory refunded;
        uint64[2] memory minGas = [uint64(0), uint64(4_600_000)];
        for (uint256 m; m < 2; ++m) {
            bytes memory env = _env(Action.Swap, 5e16, minGas[m]);
            for (uint256 gas = 2_000_000; gas < 5_000_000; gas += 30_000) {
                uint256 snap = vm.snapshotState();
                vm.prank(executor);
                try runner.fill{gas: gas}(address(usdc), 100e6, env, r) {
                    if (weth.balanceOf(user) == 0) ++refunded[m][0];
                    else ++refunded[m][1];
                } catch {}
                vm.revertToState(snap);
            }
        }
        console2.log("no minGas: refunded", refunded[0][0], "delivered", refunded[0][1]);
        console2.log("minGas 4.6M: refunded", refunded[1][0], "delivered", refunded[1][1]);
        assertGt(refunded[0][0], 0, "without it the 1/8 rule misses this depth");
        assertEq(refunded[1][0], 0, "with the signed minimum a starved route never refunds");
        assertGt(refunded[1][1], 0);
    }
}

