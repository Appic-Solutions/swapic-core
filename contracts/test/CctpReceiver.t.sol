// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "forge-std/Test.sol";
import "../src/Runner.sol";
import "../src/CctpReceiver.sol";
import "../src/SwapicTypes.sol";
import {Envelope} from "../src/libraries/Envelope.sol";
import "./mocks/RailMocks.sol";
import "./mocks/TestToken.sol";
import "./helpers/CctpPlan.sol";

contract CctpReceiverTest is Test {
    SwapicStub stub;
    TestToken usdc;
    TestToken weth;
    TransmitterMock transmitter;
    DexMock dex;
    CctpReceiver receiver;
    Runner runner;

    address user = address(0xA11CE);
    address canister = address(0x6EE);
    address stranger = address(0xBAD);
    uint64 deadline;
    uint256 nonce;

    function setUp() public {
        vm.warp(1_790_000_000);
        deadline = uint64(block.timestamp + 1 hours);
        stub = new SwapicStub();
        usdc = new TestToken();
        weth = new TestToken();
        transmitter = new TransmitterMock(IERC20(address(usdc)));
        dex = new DexMock();
        receiver = new CctpReceiver(address(stub), address(usdc), address(transmitter), CctpPlan.TOKEN_MESSENGER);
        runner = new Runner(address(stub), address(receiver), address(0), address(0));
        stub.set(false, canister, address(runner));
        usdc.transfer(address(transmitter), 1e24);
        weth.transfer(address(dex), 1e24);
    }

    function _env(Action action, uint256 minOut) internal view returns (bytes memory) {
        return Envelope.encode(
            Envelope.Data(3, keccak256("ref"), action, deadline, user, address(weth), minOut, bytes32(0), bytes32(0), 0)
        );
    }

    function _message(address mintRecipient, uint256 amount, uint256 fee, bytes memory envelope)
        internal
        returns (bytes memory)
    {
        return CctpPlan.message(
            6, 3, bytes32(++nonce), address(receiver), mintRecipient, address(usdc), amount, fee, envelope
        );
    }

    function _swap(uint256 amountIn, uint256 out) internal view returns (Recipe memory) {
        return Recipe(address(dex), abi.encodeCall(DexMock.swap, (address(usdc), address(weth), amountIn, out)), address(0));
    }

    function _empty() internal pure returns (Recipe memory) {
        return Recipe(address(0), "", address(0));
    }

    function _assertEmpty() internal view {
        assertEq(usdc.balanceOf(address(receiver)), 0, "receiver holds nothing");
        assertEq(usdc.balanceOf(address(runner)), 0, "runner holds nothing");
        assertEq(weth.balanceOf(address(runner)), 0);
        assertEq(usdc.allowance(address(receiver), address(runner)), 0);
    }

    function test_canister_completes_the_swap_on_the_minted_amount() public {
        bytes memory m = _message(address(receiver), 100e6, 1e4, _env(Action.Swap, 5e16));
        vm.prank(canister);
        receiver.completeCctp(m, "", _swap(100e6 - 1e4, 6e16));
        assertEq(weth.balanceOf(user), 6e16);
        assertEq(usdc.balanceOf(user), 0);
        _assertEmpty();
    }

    function test_the_recipient_may_complete() public {
        bytes memory m = _message(address(receiver), 100e6, 0, _env(Action.Swap, 5e16));
        vm.prank(user);
        receiver.completeCctp(m, "", _swap(100e6, 6e16));
        assertEq(weth.balanceOf(user), 6e16);
    }

    function test_a_stranger_cannot_choose_the_recipe() public {
        bytes memory m = _message(address(receiver), 100e6, 0, _env(Action.Swap, 5e16));
        vm.prank(stranger);
        vm.expectRevert(ICctpReceiver.NotCanisterOrRecipient.selector);
        receiver.completeCctp(m, "", _swap(100e6, 6e16));
    }

    function test_a_short_swap_refunds_all_usdc() public {
        bytes memory m = _message(address(receiver), 100e6, 0, _env(Action.Swap, 7e16));
        vm.prank(canister);
        receiver.completeCctp(m, "", _swap(100e6, 6e16));
        assertEq(usdc.balanceOf(user), 100e6);
        assertEq(weth.balanceOf(user), 0);
        _assertEmpty();
    }

    function test_release_after_the_deadline_only() public {
        bytes memory m = _message(address(receiver), 100e6, 0, _env(Action.Swap, 5e16));
        vm.prank(stranger);
        vm.expectRevert(ICctpReceiver.DeadlineNotPassed.selector);
        receiver.release(m, "");

        vm.warp(deadline + 1);
        vm.prank(canister);
        vm.expectRevert(ICctpReceiver.DeadlinePassed.selector);
        receiver.completeCctp(m, "", _swap(100e6, 6e16));

        stub.set(true, canister, address(runner));
        vm.expectEmit(address(receiver));
        emit ICctpReceiver.Released(keccak256("ref"), user, 100e6);
        vm.prank(stranger);
        receiver.release(m, "");
        assertEq(usdc.balanceOf(user), 100e6, "even while paused");
        _assertEmpty();
    }

    function test_paused_refuses_complete() public {
        stub.set(true, canister, address(runner));
        bytes memory m = _message(address(receiver), 100e6, 0, _env(Action.Swap, 5e16));
        vm.prank(canister);
        vm.expectRevert(ICctpReceiver.IsPaused.selector);
        receiver.completeCctp(m, "", _swap(100e6, 6e16));
    }

    function test_refusals() public {
        bytes memory elsewhere = _message(stranger, 100e6, 0, _env(Action.Swap, 5e16));
        vm.startPrank(canister);
        vm.expectRevert(ICctpReceiver.BadMessage.selector);
        receiver.completeCctp(elsewhere, "", _swap(100e6, 6e16));

        bytes memory m = _message(address(receiver), 100e6, 0, _env(Action.Swap, 5e16));
        vm.expectRevert(ICctpReceiver.BadMessage.selector);
        receiver.completeCctp(bytes.concat(m, hex"00"), "", _swap(100e6, 6e16));

        bytes memory deliver = _message(address(receiver), 100e6, 0, _env(Action.Deliver, 0));
        vm.expectRevert(ICctpReceiver.WrongAction.selector);
        receiver.completeCctp(deliver, "", _empty());

        bytes memory ck = _message(address(receiver), 100e6, 0, _env(Action.CkDeposit, 0));
        vm.expectRevert(ICctpReceiver.BadRecipe.selector);
        receiver.completeCctp(ck, "", _swap(100e6, 6e16));
        Recipe memory onlyScaler = _empty();
        onlyScaler.scaler = address(dex);
        vm.expectRevert(ICctpReceiver.BadRecipe.selector);
        receiver.completeCctp(ck, "", onlyScaler);

        bytes memory zeroMin = _message(address(receiver), 100e6, 0, _env(Action.Swap, 0));
        vm.expectRevert(IRunner.BadArrival.selector);
        receiver.completeCctp(zeroMin, "", _swap(100e6, 6e16));
        vm.stopPrank();
    }

    function test_ck_deposit_without_a_helper_refunds() public {
        bytes memory m = _message(address(receiver), 100e6, 0, _env(Action.CkDeposit, 0));
        vm.prank(canister);
        receiver.completeCctp(m, "", _empty());
        assertEq(usdc.balanceOf(user), 100e6);
        _assertEmpty();
    }

    function test_envelope_of() public {
        bytes memory m = _message(address(receiver), 100e6, 0, _env(Action.Swap, 5e16));
        Envelope.Data memory e = receiver.envelopeOf(m);
        assertEq(e.recipient, user);
        assertEq(e.minOut, 5e16);
        assertEq(uint8(e.action), uint8(Action.Swap));
        assertEq(receiver.claimable(m).minOut, 5e16);
    }

    function test_claimable_applies_the_same_checks_as_complete() public {
        bytes memory deliver = _message(user, 100e6, 0, _env(Action.Deliver, 0));
        assertEq(receiver.envelopeOf(deliver).recipient, user);
        vm.expectRevert(ICctpReceiver.BadMessage.selector);
        receiver.claimable(deliver);
    }

    // ---- Review Focus 2 ----
    function test_donation_does_not_block_cctp() public {
        usdc.transfer(address(receiver), 1);
        usdc.transfer(address(runner), 1);
        weth.transfer(address(runner), 1);
        bytes memory m = _message(address(receiver), 100e6, 0, _env(Action.Swap, 5e16));
        vm.prank(canister);
        receiver.completeCctp(m, "", _swap(100e6, 6e16));
        assertEq(weth.balanceOf(user), 6e16);
        assertEq(usdc.balanceOf(user), 0);

        bytes memory r = _message(address(receiver), 50e6, 0, _env(Action.Swap, 5e16));
        vm.warp(deadline + 1);
        receiver.release(r, "");
        assertEq(usdc.balanceOf(user), 50e6, "exactly the mint");

        assertEq(usdc.balanceOf(address(receiver)), 1);
        assertEq(usdc.balanceOf(address(runner)), 1);
        assertEq(weth.balanceOf(address(runner)), 1);
    }

    /// A starved completion reverts whole (the message stays unclaimed), so
    /// the canister can retry with more gas.
    function test_a_starved_completion_reverts_and_can_be_retried() public {
        HungryDex hungry = new HungryDex();
        weth.transfer(address(hungry), 1e20);
        Recipe memory r = Recipe(
            address(hungry),
            abi.encodeCall(HungryDex.swap, (address(usdc), address(weth), 100e6, 6e16, 30_000, 3)),
            address(0)
        );
        bytes memory m = _message(address(receiver), 100e6, 0, _env(Action.Swap, 5e16));
        vm.prank(canister);
        vm.expectRevert(IRunner.OutOfGas.selector);
        receiver.completeCctp{gas: 3_000_000}(m, "", r);
        vm.prank(canister);
        receiver.completeCctp{gas: 30_000_000}(m, "", r);
        assertEq(weth.balanceOf(user), 6e16);
        assertEq(usdc.balanceOf(user), 0);
        _assertEmpty();
    }

    /// A generic CCTP message (header recipient: the attacker's handler) shaped
    /// like a burn to the receiver, whose handler relays a victim's
    /// caller-zero mint to the receiver inside the receive: refused before any
    /// call, so the relayed mint cannot be acted on under the attacker's
    /// envelope.
    function test_a_generic_message_relaying_a_caller_zero_mint_is_refused() public {
        bytes memory victimBurn = CctpPlan.message(
            6, 3, keccak256("victim"), address(0), address(receiver), address(usdc), 100e6, 0, _env(Action.Swap, 5e16)
        );
        RelayHandler handler = new RelayHandler(transmitter);
        handler.arm(victimBurn);

        bytes memory attackerEnv = Envelope.encode(
            Envelope.Data(3, keccak256("x"), Action.Swap, deadline, stranger, address(weth), 1, bytes32(0), bytes32(0), 0)
        );
        bytes memory full = CctpPlan.message(
            6, 3, keccak256("generic"), address(receiver), address(receiver), address(usdc), 1, 0, attackerEnv
        );
        // the same bytes with the header recipient (76..108) set to the handler
        bytes memory generic = bytes.concat(
            _slice(full, 0, 76), bytes32(uint256(uint160(address(handler)))), _slice(full, 108, full.length)
        );
        Recipe memory failing = Recipe(address(dex), hex"deadbeef", address(0));
        vm.prank(stranger);
        vm.expectRevert(ICctpReceiver.BadMessage.selector);
        receiver.completeCctp(generic, "", failing);
        assertEq(usdc.balanceOf(stranger), 0, "nothing stolen");
        assertFalse(transmitter.usedNonces(keccak256("victim")), "the victim's burn is still unclaimed");
    }

    function test_a_caller_zero_burn_is_refused() public {
        bytes memory m = CctpPlan.message(
            6, 3, keccak256("zero"), address(0), address(receiver), address(usdc), 100e6, 0, _env(Action.Swap, 5e16)
        );
        vm.prank(canister);
        vm.expectRevert(ICctpReceiver.BadMessage.selector);
        receiver.completeCctp(m, "", _swap(100e6, 6e16));
        vm.warp(deadline + 1);
        vm.expectRevert(ICctpReceiver.BadMessage.selector);
        receiver.release(m, "");
    }

    function test_nothing_minted_is_refused() public {
        bytes memory m = _message(address(receiver), 100e6, 100e6, _env(Action.Swap, 5e16));
        vm.prank(canister);
        vm.expectRevert(ICctpReceiver.NothingMinted.selector);
        receiver.completeCctp(m, "", _swap(0, 0));
    }

    function _slice(bytes memory b, uint256 from, uint256 to) internal pure returns (bytes memory out) {
        out = new bytes(to - from);
        for (uint256 i; i < out.length; ++i) {
            out[i] = b[from + i];
        }
    }
}

