// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "./helpers/RunnerBase.sol";

contract RunnerTest is RunnerBase {
    function _threeSteps() internal view returns (Order memory o) {
        o = _order(address(weth), 1e18, 3);
        o.steps[0] = _swapStep(address(weth), address(usdc), 1e18, 2500e6);
        o.steps[1] = _feeStep(1e6);
        o.steps[2] = _burnStep(2400e6, 0);
    }

    function test_three_steps_leave_the_runner_empty() public {
        _exec(_threeSteps());
        (uint256 burned,,,,,,,) = messenger.last();
        assertEq(burned, 2499e6, "the burn is the whole delta, written in");
        assertEq(usdc.balanceOf(canister), 1e6);
        assertEq(usdc.balanceOf(address(runner)), 0);
        assertEq(weth.balanceOf(address(runner)), 0);
        assertEq(weth.allowance(address(runner), address(dex)), 0, "approval reset");
        assertEq(usdc.allowance(address(runner), address(messenger)), 0, "approval reset");
    }

    function test_writes_minus_at_unaligned_offset() public {
        Order memory o = _order(address(usdc), 100e6, 2);
        bytes memory inner = abi.encodeWithSelector(bytes4(0x12345678), user, uint256(0));
        bytes memory data = abi.encodeCall(NestedRailMock.pullNested, (address(usdc), inner));
        // selector, token, offset, length, inner selector, inner address
        uint32 at = 4 + 32 + 32 + 32 + 4 + 32;
        assertEq(at % 32, 8, "not a word boundary");
        o.steps[0] = Step(address(nested), data, address(usdc), 100e6, _write(at, 57_115));
        o.steps[1] = _feeStep(57_115);
        _exec(o);
        assertEq(nested.last(), 100e6 - 57_115);
        assertEq(usdc.balanceOf(canister), 57_115);
        assertEq(usdc.balanceOf(address(runner)), 0);
    }

    function test_min_short_reverts() public {
        Order memory o = _order(address(usdc), 100e6, 1);
        o.steps[0] = _burnStep(100e6 + 1, 0);
        uint256 b = _pull(o);
        vm.expectRevert(IRunner.Short.selector);
        _run(o, b);
    }

    function test_write_out_of_bounds_reverts() public {
        Order memory o = _order(address(usdc), 100e6, 1);
        o.steps[0] = _burnStep(0, 0);
        o.steps[0].writes[0].at = uint32(o.steps[0].data.length - 31);
        uint256 b = _pull(o);
        vm.expectRevert(IRunner.BadWrite.selector);
        _run(o, b);
    }

    function test_write_below_minus_reverts() public {
        Order memory o = _order(address(usdc), 100e6, 1);
        o.steps[0] = _burnStep(0, 100e6 + 1);
        uint256 b = _pull(o);
        vm.expectRevert(IRunner.BadWrite.selector);
        _run(o, b);
    }

    function test_writes_on_token_zero_revert() public {
        Order memory o = _order(address(usdc), 100e6, 1);
        o.steps[0] = _feeStep(100e6);
        o.steps[0].writes = _write(36, 0);
        uint256 b = _pull(o);
        vm.expectRevert(IRunner.BadWrite.selector);
        _run(o, b);
    }

    function test_same_chain_payout() public {
        Order memory o = _order(address(weth), 1e18, 1);
        o.steps[0] = _swapStep(address(weth), address(usdc), 1e18, 2500e6);
        o.payout = Payout(address(usdc), user, 2500e6);
        uint256 b = _pull(o);
        vm.expectEmit(address(runner));
        emit IRunner.PaidOut(keccak256("id"), user, address(usdc), 2500e6);
        _run(o, b);
        assertEq(usdc.balanceOf(user), 2500e6);
        assertEq(usdc.balanceOf(address(runner)), 0);
    }

    /// A fee-on-transfer tokenOut: the Runner's delta passes minOut, what the
    /// recipient receives does not.
    function test_payout_counts_what_the_recipient_received() public {
        FeeToken fee = new FeeToken();
        fee.transfer(address(dex), 1e24);
        Order memory o = _order(address(weth), 1e18, 1);
        o.steps[0] = _swapStep(address(weth), address(fee), 1e18, 1000e18);
        // the Runner gets 990, the recipient 980.1
        o.payout = Payout(address(fee), user, 990e18);
        uint256 b = _pull(o);
        vm.expectRevert(IRunner.Short.selector);
        _run(o, b);

        o.payout.minOut = 980e18;
        _run(o, b);
        assertEq(fee.balanceOf(user), 980.1e18);
    }

    function test_payout_to_nowhere_or_to_us_is_refused() public {
        address[4] memory bad = [address(0), address(stub), address(runner), receiver];
        for (uint256 i; i < bad.length; ++i) {
            Order memory o = _order(address(weth), 1e18, 1);
            o.steps[0] = _swapStep(address(weth), address(usdc), 1e18, 2500e6);
            o.payout = Payout(address(usdc), bad[i], 1);
            uint256 b = _pull(o);
            vm.expectRevert(IRunner.BadRecipient.selector);
            _run(o, b);
            o.payout.token = NATIVE;
            vm.expectRevert(IRunner.BadRecipient.selector);
            _run(o, b);
            vm.prank(address(runner));
            weth.transfer(address(this), 1e18);
        }
    }

    function test_payout_below_min_out_reverts() public {
        Order memory o = _order(address(weth), 1e18, 1);
        o.steps[0] = _swapStep(address(weth), address(usdc), 1e18, 2500e6);
        o.payout = Payout(address(usdc), user, 2500e6 + 1);
        uint256 b = _pull(o);
        vm.expectRevert(IRunner.Short.selector);
        _run(o, b);
    }

    function test_native_token_in() public {
        Order memory o = _order(NATIVE, 1 ether, 1);
        o.steps[0] = _swapStep(address(wrapped), address(usdc), 1 ether, 2500e6);
        o.payout = Payout(address(usdc), user, 2500e6);
        _exec(o);
        assertEq(usdc.balanceOf(user), 2500e6);
        assertEq(address(runner).balance, 0);
        assertEq(wrapped.balanceOf(address(runner)), 0);
    }

    /// The review's case: a rail that pushes stray ether back to its caller
    /// (Eco's excessNative) no longer breaks an ether-in order, since the
    /// steps run on the wrapped token and ether is not tracked.
    function test_stray_ether_pushed_in_does_not_block_native_in() public {
        ExcessNativeRail rail = new ExcessNativeRail();
        vm.deal(address(rail), 1);
        Order memory o = _order(NATIVE, 1 ether, 1);
        o.steps[0] = Step(
            address(rail), abi.encodeCall(ExcessNativeRail.take, (address(wrapped), 0)), address(wrapped), 1 ether, _write(36, 0)
        );
        _exec(o);
        assertEq(wrapped.balanceOf(address(rail)), 1 ether);
        assertEq(address(runner).balance, 1, "the planted wei, untracked");
    }

    function test_native_payout() public {
        Order memory o = _order(address(usdc), 2500e6, 1);
        o.steps[0] = Step(
            address(dex),
            abi.encodeCall(DexMock.swapToEth, (address(usdc), 2500e6, 1 ether)),
            address(usdc),
            2500e6,
            _noWrites()
        );
        o.payout = Payout(NATIVE, user, 1 ether);
        _exec(o);
        assertEq(user.balance, 1 ether);
        assertEq(address(runner).balance, 0);
        assertEq(usdc.balanceOf(address(runner)), 0);
    }

    function test_native_payout_to_a_rejecter_reverts() public {
        Order memory o = _order(address(usdc), 2500e6, 1);
        o.steps[0] = Step(
            address(dex),
            abi.encodeCall(DexMock.swapToEth, (address(usdc), 2500e6, 1 ether)),
            address(usdc),
            2500e6,
            _noWrites()
        );
        o.payout = Payout(NATIVE, address(new RejectsEth()), 1 ether);
        uint256 b = _pull(o);
        vm.expectRevert(IRunner.CallFailed.selector);
        _run(o, b);
    }

    function test_only_swapic() public {
        Order memory o = _threeSteps();
        vm.expectRevert(IRunner.OnlySwapic.selector);
        runner.execute(o, bytes32(0), 0);
    }

    // ---- Review Focus 1: leftover anywhere ----

    function test_execute_partial_spend_reverts() public {
        Order memory o = _threeSteps();
        o.steps[0].data = abi.encodeCall(DexMock.swapShort, (address(weth), address(usdc), 1e18, 2500e6));
        uint256 b = _pull(o);
        vm.expectRevert(IRunner.LeftOver.selector);
        _run(o, b);
        assertEq(weth.balanceOf(address(runner)), 1e18, "reverted whole: only the test's pull is there");
    }

    function test_execute_partial_burn_reverts() public {
        Order memory o = _threeSteps();
        o.steps[2] = _burnStep(0, 1);
        uint256 b = _pull(o);
        vm.expectRevert(IRunner.LeftOver.selector);
        _run(o, b);
    }

    // ---- Review Focus 2: donations ----

    function test_donation_does_not_block_execute() public {
        weth.transfer(address(runner), 1);
        usdc.transfer(address(runner), 1);
        vm.deal(address(runner), 1);
        _exec(_threeSteps());
        (uint256 burned,,,,,,,) = messenger.last();
        assertEq(burned, 2499e6, "the donation is not burned");
        assertEq(weth.balanceOf(address(runner)), 1);
        assertEq(usdc.balanceOf(address(runner)), 1);
        assertEq(address(runner).balance, 1);
    }

    function test_donation_does_not_block_native() public {
        vm.deal(address(runner), 1);
        usdc.transfer(address(runner), 1);
        vm.deal(address(this), 1);
        wrapped.deposit{value: 1}();
        wrapped.transfer(address(runner), 1);
        Order memory o = _order(NATIVE, 1 ether, 1);
        o.steps[0] = _swapStep(address(wrapped), address(usdc), 1 ether, 2500e6);
        o.payout = Payout(address(usdc), user, 2500e6);
        _exec(o);
        assertEq(usdc.balanceOf(user), 2500e6, "exactly the swap output");
        assertEq(address(runner).balance, 1);
        assertEq(wrapped.balanceOf(address(runner)), 1);
        assertEq(usdc.balanceOf(address(runner)), 1);
    }

    // ---- Review Focus 3: draining a victim through the Runner ----

    function _attack(Step memory s) internal {
        Order memory o = _order(address(usdc), 1, 1);
        o.payer = attacker;
        o.steps[0] = s;
        uint256 b = _pull(o);
        vm.expectRevert(IRunner.BadStep.selector);
        _run(o, b);
    }

    function test_runner_refuses_forbidden_calls() public {
        usdc.transfer(victim, 1000e6);
        vm.prank(victim);
        usdc.approve(address(runner), type(uint256).max);
        Write[] memory none = _noWrites();
        address u = address(usdc);

        _attack(Step(u, abi.encodeCall(IERC20.transferFrom, (victim, attacker, 1000e6)), address(0), 0, none));
        _attack(Step(u, abi.encodeCall(IERC20.approve, (attacker, type(uint256).max)), address(0), 0, none));
        _attack(Step(u, abi.encodeWithSelector(0x39509351, attacker, type(uint256).max), address(0), 0, none));
        _attack(
            Step(
                u,
                abi.encodeWithSelector(
                    0xd505accf, address(runner), attacker, type(uint256).max, 0, uint8(0), bytes32(0), bytes32(0)
                ),
                address(0),
                0,
                none
            )
        );
        _attack(Step(u, abi.encodeWithSelector(0xa22cb465, attacker, true), address(0), 0, none));
        _attack(Step(u, abi.encodeWithSelector(0xd73dd623, attacker, type(uint256).max), address(0), 0, none));
        _attack(Step(u, abi.encodeWithSelector(0x959b8c3f, attacker), address(0), 0, none));
        bytes4[11] memory more = _moreGrants();
        for (uint256 i; i < more.length; ++i) {
            _attack(Step(u, abi.encodeWithSelector(more[i], attacker, uint256(1), uint256(1)), address(0), 0, none));
        }
        // a token step may not approve either: the selector is checked whatever the token
        _attack(Step(u, abi.encodeCall(IERC20.approve, (attacker, 1)), u, 0, none));

        bytes memory harmless = abi.encodeCall(IERC20.transfer, (attacker, 1));
        _attack(Step(address(stub), harmless, address(0), 0, none));
        _attack(Step(address(runner), harmless, address(0), 0, none));
        _attack(Step(receiver, harmless, address(0), 0, none));
        vm.etch(PERMIT2, address(dex).code);
        _attack(Step(PERMIT2, harmless, address(0), 0, none));
        _attack(Step(address(0xC0DE1E55), harmless, address(0), 0, none));

        assertEq(usdc.balanceOf(victim), 1000e6, "the victim keeps everything");
        assertEq(usdc.balanceOf(attacker), 0);
    }

    /// An attacker's own token gives a delta whose top bytes are a banned
    /// selector, written over the selector: the guard sees the final data.
    function test_a_write_cannot_forge_a_banned_selector() public {
        usdc.transfer(victim, 1000e6);
        vm.prank(victim);
        usdc.approve(address(runner), type(uint256).max);
        TestToken junk = new TestToken();
        // the selector, then the top 28 bytes of the first argument word: the
        // final data is exactly transferFrom(victim, attacker, 1000e6)
        uint256 big = (uint256(0x23b872dd) << 224) | (uint256(uint160(victim)) >> 32);

        Order memory o = _order(address(usdc), 1, 2);
        o.payer = attacker;
        o.steps[0] = Step(
            address(junk), abi.encodeCall(TestToken.mint, (address(runner), big)), address(0), 0, _noWrites()
        );
        bytes memory data = abi.encodeCall(IERC20.transferFrom, (victim, attacker, 1000e6));
        data[0] = 0;
        data[1] = 0;
        data[2] = 0;
        data[3] = 0;
        o.steps[1] = Step(address(usdc), data, address(junk), 0, _write(0, 0));
        uint256 b = _pull(o);
        vm.expectRevert(IRunner.BadStep.selector);
        _run(o, b);
        assertEq(usdc.balanceOf(victim), 1000e6);
    }

    /// ERC-1363 (both), DSToken, ERC-6909 (both), ERC-1820 (both), Comet,
    /// Morpho, Aave, Balancer, Maker: selectors from `cast sig`.
    function _moreGrants() internal pure returns (bytes4[11] memory) {
        return [
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
            0xfa6e671d
        ];
    }

    function test_maker_hope_is_refused() public {
        Order memory o = _order(address(usdc), 1, 1);
        o.steps[0] = Step(address(usdc), abi.encodeWithSelector(0xa3b22fc4, attacker), address(0), 0, _noWrites());
        uint256 b = _pull(o);
        vm.expectRevert(IRunner.BadStep.selector);
        _run(o, b);
    }
}

