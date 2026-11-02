// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "./helpers/RunnerBase.sol";
import {Envelope} from "../src/libraries/Envelope.sol";

/// A scaler that answers with whatever calldata it was built with.
contract FixedScaler {
    bytes public out;

    constructor(bytes memory out_) {
        out = out_;
    }

    function getScaledInputData(bytes calldata, uint256) external view returns (bool, bytes memory) {
        return (true, out);
    }
}

contract FillTest is RunnerBase {
    address executor = address(0xEC0E);
    bytes32 constant REF = keccak256("ref");

    function setUp() public override {
        super.setUp();
        usdc.transfer(executor, 1e24);
    }

    function _env(Action action, address tokenOut, uint256 minOut) internal view returns (bytes memory) {
        return Envelope.encode(
            Envelope.Data({
                version: 3,
                ref: REF,
                action: action,
                deadline: uint64(block.timestamp + 1 hours),
                recipient: user,
                tokenOut: tokenOut,
                minOut: minOut,
                principal: bytes32(uint256(0xbeef)),
                subaccount: bytes32(uint256(7)),
                minGas: 0
            })
        );
    }

    function _recipe(uint256 amountIn, uint256 out) internal view returns (Recipe memory) {
        return Recipe(address(dex), abi.encodeCall(DexMock.swap, (address(usdc), address(weth), amountIn, out)), address(0));
    }

    function _fill(uint256 amount, bytes memory envelope, Recipe memory r) internal {
        _approve(amount);
        _doFill(amount, envelope, r);
    }

    function _approve(uint256 amount) internal {
        vm.prank(executor);
        usdc.approve(address(runner), amount);
    }

    function _doFill(uint256 amount, bytes memory envelope, Recipe memory r) internal {
        vm.prank(executor);
        runner.fill(address(usdc), amount, envelope, r);
    }

    function _assertRefundedAll(uint256 amount) internal view {
        assertEq(usdc.balanceOf(user), amount, "all of the arrival, in USDC");
        assertEq(weth.balanceOf(user), 0, "no tokenOut");
        assertEq(user.balance, 0, "no ether");
        assertEq(usdc.balanceOf(address(runner)), 0);
        assertEq(usdc.allowance(executor, address(runner)), 0, "nothing left in the executor");
    }

    function test_full_swap_pays_only_token_out() public {
        _approve(100e6);
        vm.expectEmit(address(runner));
        emit IRunner.Delivered(REF, user, address(weth), 100e6, 6e16);
        _doFill(100e6, _env(Action.Swap, address(weth), 5e16), _recipe(100e6, 6e16));
        assertEq(weth.balanceOf(user), 6e16);
        assertEq(usdc.balanceOf(user), 0, "no USDC beside the token");
        assertEq(usdc.balanceOf(address(runner)), 0);
        assertEq(weth.balanceOf(address(runner)), 0);
        assertEq(usdc.allowance(address(runner), address(dex)), 0);
    }

    function test_short_output_refunds_all() public {
        _approve(100e6);
        vm.expectEmit(address(runner));
        emit IRunner.Refunded(REF, user, address(usdc), 100e6);
        _doFill(100e6, _env(Action.Swap, address(weth), 7e16), _recipe(100e6, 6e16));
        _assertRefundedAll(100e6);
    }

    // ---- Review Focus 1 ----
    function test_arrival_partial_spend_refunds_all() public {
        Recipe memory r = Recipe(
            address(dex), abi.encodeCall(DexMock.swapShort, (address(usdc), address(weth), 100e6, 6e16)), address(0)
        );
        _fill(100e6, _env(Action.Swap, address(weth), 5e16), r);
        _assertRefundedAll(100e6);
        assertEq(usdc.allowance(address(runner), address(dex)), 0);
    }

    /// A fee-on-transfer tokenOut whose delivery to the recipient falls under
    /// minOut refunds all of the arrival.
    function test_delivery_counts_what_the_recipient_received() public {
        FeeToken fee = new FeeToken();
        fee.transfer(address(dex), 1e24);
        Recipe memory r =
            Recipe(address(dex), abi.encodeCall(DexMock.swap, (address(usdc), address(fee), 100e6, 1000e18)), address(0));
        _fill(100e6, _env(Action.Swap, address(fee), 990e18), r);
        assertEq(usdc.balanceOf(user), 100e6, "refunded all");
        assertEq(fee.balanceOf(user), 0);

        _fill(100e6, _env(Action.Swap, address(fee), 980e18), r);
        assertEq(fee.balanceOf(user), 980.1e18, "delivered");
    }

    function test_arrival_to_nowhere_or_to_us_is_refused() public {
        address[4] memory bad = [address(0), address(stub), address(runner), receiver];
        _approve(100e6);
        for (uint256 i; i < bad.length; ++i) {
            bytes memory env = Envelope.encode(
                Envelope.Data(3, REF, Action.Swap, uint64(block.timestamp + 1 hours), bad[i], address(weth), 1, 0, 0, 0)
            );
            vm.prank(executor);
            vm.expectRevert(IRunner.BadRecipient.selector);
            runner.fill(address(usdc), 100e6, env, _recipe(100e6, 6e16));
            env = Envelope.encode(
                Envelope.Data(3, REF, Action.CkDeposit, uint64(block.timestamp + 1 hours), bad[i], address(0), 0, 0, 0, 0)
            );
            vm.prank(executor);
            vm.expectRevert(IRunner.BadRecipient.selector);
            runner.fill(address(usdc), 100e6, env, _recipe(100e6, 6e16));
        }
    }

    function test_scaler_rescales_to_what_arrived() public {
        Recipe memory r = _recipe(50e6, 3e16);
        r.scaler = address(new ScalerMock(true));
        _fill(100e6, _env(Action.Swap, address(weth), 5e16), r);
        assertEq(weth.balanceOf(user), 6e16);
        assertEq(usdc.balanceOf(user), 0);
        assertEq(usdc.balanceOf(address(runner)), 0);
    }

    function test_scaler_failure_refunds_all() public {
        Recipe memory r = _recipe(50e6, 3e16);
        r.scaler = address(new ScalerMock(false));
        _fill(100e6, _env(Action.Swap, address(weth), 5e16), r);
        _assertRefundedAll(100e6);
    }

    function test_native_token_out() public {
        Recipe memory r = Recipe(
            address(dex), abi.encodeCall(DexMock.swapToEth, (address(usdc), 100e6, 1 ether)), address(0)
        );
        _fill(100e6, _env(Action.Swap, NATIVE, 9e17), r);
        assertEq(user.balance, 1 ether);
        assertEq(usdc.balanceOf(user), 0);
        assertEq(address(runner).balance, 0);
    }

    function test_recipient_that_rejects_eth_gets_all_usdc() public {
        address rejecter = address(new RejectsEth());
        Recipe memory r = Recipe(
            address(dex), abi.encodeCall(DexMock.swapToEth, (address(usdc), 100e6, 1 ether)), address(0)
        );
        bytes memory env = Envelope.encode(
            Envelope.Data(3, REF, Action.Swap, uint64(block.timestamp + 1 hours), rejecter, NATIVE, 9e17, 0, 0, 0)
        );
        _fill(100e6, env, r);
        assertEq(usdc.balanceOf(rejecter), 100e6);
        assertEq(rejecter.balance, 0);
        assertEq(address(runner).balance, 0);
    }

    function test_ck_deposit_with_a_helper() public {
        runner = _deployRunner(address(helper));
        _approve(100e6);
        vm.expectEmit(address(runner));
        emit IRunner.CkDeposited(REF, bytes32(uint256(0xbeef)), bytes32(uint256(7)), 100e6);
        _doFill(100e6, _env(Action.CkDeposit, address(0), 0), Recipe(address(0), "", address(0)));
        assertEq(helper.lastAmount(), 100e6);
        assertEq(helper.lastPrincipal(), bytes32(uint256(0xbeef)));
        assertEq(usdc.balanceOf(address(helper)), 100e6);
        assertEq(usdc.balanceOf(address(runner)), 0);
        assertEq(usdc.allowance(address(runner), address(helper)), 0);
    }

    function test_ck_deposit_without_a_helper_refunds() public {
        _fill(100e6, _env(Action.CkDeposit, address(0), 0), Recipe(address(0), "", address(0)));
        _assertRefundedAll(100e6);
    }

    function test_deliver_envelope_reverts() public {
        vm.startPrank(executor);
        usdc.approve(address(runner), 100e6);
        vm.expectRevert(IRunner.WrongAction.selector);
        runner.fill(address(usdc), 100e6, _env(Action.Deliver, address(0), 0), _recipe(100e6, 6e16));
        vm.stopPrank();
    }

    function test_bad_envelope_reverts() public {
        vm.startPrank(executor);
        usdc.approve(address(runner), 100e6);
        vm.expectRevert(Envelope.BadEnvelope.selector);
        runner.fill(address(usdc), 100e6, hex"00", _recipe(100e6, 6e16));
        vm.stopPrank();
    }

    function test_paused_reverts() public {
        stub.set(true, canister, address(runner));
        vm.startPrank(executor);
        usdc.approve(address(runner), 100e6);
        vm.expectRevert(IRunner.IsPaused.selector);
        runner.fill(address(usdc), 100e6, _env(Action.Swap, address(weth), 5e16), _recipe(100e6, 6e16));
        vm.stopPrank();
    }

    function test_self_calls_only() public {
        Envelope.Data memory e;
        vm.expectRevert(IRunner.OnlySelf.selector);
        runner.swapAll(address(usdc), 1, 0, e, _recipe(1, 1));
        vm.expectRevert(IRunner.OnlySelf.selector);
        runner.ckDepositAll(address(usdc), 1, 0, e);
    }

    function test_zero_minout_or_same_token_rejected() public {
        vm.startPrank(executor);
        usdc.approve(address(runner), 100e6);
        vm.expectRevert(IRunner.BadArrival.selector);
        runner.fill(address(usdc), 100e6, _env(Action.Swap, address(weth), 0), _recipe(100e6, 6e16));
        vm.expectRevert(IRunner.BadArrival.selector);
        runner.fill(address(usdc), 100e6, _env(Action.Swap, address(0), 5e16), _recipe(100e6, 6e16));
        vm.expectRevert(IRunner.BadArrival.selector);
        runner.fill(address(usdc), 100e6, _env(Action.Swap, address(usdc), 5e16), _recipe(100e6, 6e16));
        vm.stopPrank();

        // the recipe may not target the arriving token: undone, all refunded
        Recipe memory r = Recipe(address(usdc), abi.encodeCall(IERC20.transfer, (attacker, 100e6)), address(0));
        _fill(100e6, _env(Action.Swap, address(weth), 5e16), r);
        _assertRefundedAll(100e6);
        assertEq(usdc.balanceOf(attacker), 0);
        vm.prank(address(runner));
        vm.expectRevert(IRunner.BadStep.selector);
        runner.swapAll(address(usdc), 100e6, 0, abi.decode(_env(Action.Swap, address(weth), 5e16), (Envelope.Data)), r);
    }

    // ---- Review Focus 2 ----
    function test_donation_does_not_block_fill() public {
        usdc.transfer(address(runner), 1);
        weth.transfer(address(runner), 1);
        vm.deal(address(runner), 1);
        _fill(100e6, _env(Action.Swap, address(weth), 5e16), _recipe(100e6, 6e16));
        assertEq(weth.balanceOf(user), 6e16, "exactly the swap output");
        assertEq(usdc.balanceOf(user), 0);

        _fill(100e6, _env(Action.Swap, address(weth), 7e16), _recipe(100e6, 6e16));
        assertEq(usdc.balanceOf(user), 100e6, "exactly the arrival, not the donation");

        assertEq(usdc.balanceOf(address(runner)), 1);
        assertEq(weth.balanceOf(address(runner)), 1);
        assertEq(address(runner).balance, 1);
    }

    // ---- Review Focus 3 ----
    function _refused(Recipe memory r) internal {
        uint256 before = usdc.balanceOf(user);
        _fill(100e6, _env(Action.Swap, address(weth), 5e16), r);
        assertEq(usdc.balanceOf(user) - before, 100e6, "refunded whole");
        assertEq(weth.balanceOf(user), 0);

        Envelope.Data memory e = abi.decode(_env(Action.Swap, address(weth), 5e16), (Envelope.Data));
        usdc.transfer(address(runner), 100e6);
        vm.prank(address(runner));
        vm.expectRevert(IRunner.BadStep.selector);
        runner.swapAll(address(usdc), 100e6, 0, e, r);
        vm.prank(address(runner));
        usdc.transfer(address(this), 100e6);
    }

    /// The watcher's own path when no swap is found for a CCTP arrival: a recipe whose step
    /// targets the arriving USDC itself. The Runner refuses that step, and the fill pays
    /// all the USDC to the recipient at once.
    function test_a_recipe_on_the_arriving_usdc_pays_all_usdc() public {
        Recipe memory r = Recipe(address(usdc), "", address(0));
        _approve(100e6);
        vm.expectEmit(address(runner));
        emit IRunner.Refunded(REF, user, address(usdc), 100e6);
        _doFill(100e6, _env(Action.Swap, address(weth), 5e16), r);
        _assertRefundedAll(100e6);

        Envelope.Data memory e = abi.decode(_env(Action.Swap, address(weth), 5e16), (Envelope.Data));
        usdc.transfer(address(runner), 100e6);
        vm.prank(address(runner));
        vm.expectRevert(IRunner.BadStep.selector);
        runner.swapAll(address(usdc), 100e6, 0, e, r);
        vm.prank(address(runner));
        usdc.transfer(address(this), 100e6);
    }

    function test_recipe_refuses_forbidden_calls() public {
        weth.transfer(victim, 1000e18);
        vm.prank(victim);
        weth.approve(address(runner), type(uint256).max);
        address w = address(weth);

        _refused(Recipe(w, abi.encodeCall(IERC20.transferFrom, (victim, attacker, 1000e18)), address(0)));
        _refused(Recipe(w, abi.encodeCall(IERC20.approve, (attacker, type(uint256).max)), address(0)));
        _refused(Recipe(w, abi.encodeWithSelector(0x39509351, attacker, type(uint256).max), address(0)));
        _refused(
            Recipe(
                w,
                abi.encodeWithSelector(
                    0xd505accf, address(runner), attacker, type(uint256).max, 0, uint8(0), bytes32(0), bytes32(0)
                ),
                address(0)
            )
        );
        _refused(Recipe(w, abi.encodeWithSelector(0xa22cb465, attacker, true), address(0)));
        _refused(Recipe(w, abi.encodeWithSelector(0xd73dd623, attacker, type(uint256).max), address(0)));
        _refused(Recipe(w, abi.encodeWithSelector(0x959b8c3f, attacker), address(0)));
        bytes4[12] memory more = [
            bytes4(0x3177029f),
            0xcae9ca51,
            0xdaea85c5,
            0x558a7297,
            0x426a8493,
            0x5df8122f,
            0x29965a1d,
            0x110496e5,
            0xeecea000,
            0xc04a8a10,
            0xfa6e671d,
            0xa3b22fc4
        ];
        for (uint256 i; i < more.length; ++i) {
            _refused(Recipe(w, abi.encodeWithSelector(more[i], attacker, uint256(1), uint256(1)), address(0)));
        }
        // a scaler that turns harmless data into a transferFrom
        address evil = address(new FixedScaler(abi.encodeCall(IERC20.transferFrom, (victim, attacker, 1000e18))));
        _refused(Recipe(w, abi.encodeCall(IERC20.transfer, (attacker, 1)), evil));

        bytes memory swapData = abi.encodeCall(DexMock.swap, (address(usdc), address(weth), 100e6, 6e16));
        _refused(Recipe(address(stub), swapData, address(0)));
        _refused(Recipe(address(runner), swapData, address(0)));
        _refused(Recipe(receiver, swapData, address(0)));
        vm.etch(PERMIT2, address(dex).code);
        _refused(Recipe(PERMIT2, swapData, address(0)));
        _refused(Recipe(address(0xC0DE1E55), swapData, address(0)));

        assertEq(weth.balanceOf(victim), 1000e18, "the victim keeps everything");
        assertEq(weth.balanceOf(attacker), 0);
        assertEq(weth.allowance(address(runner), attacker), 0);
    }
}
