// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "forge-std/Test.sol";
import "../src/SwapicExecutor.sol";
import "../src/SwapicTypes.sol";
import {HookData} from "../src/libraries/HookData.sol";
import "./mocks/RailMocks.sol";
import "./mocks/TestToken.sol";

contract SwapicExecutorTest is Test {
    SwapicExecutor executor;
    TestToken usdc;
    TestToken weth;
    TransmitterMock transmitter;
    DexMock dex;

    address user = address(0xA11CE);
    address keeper = address(0x6EE);
    address stranger = address(0xBAD);
    address ecoExecutor = address(0xEC0E);
    uint64 deadline;

    function setUp() public {
        vm.warp(1_790_000_000);
        deadline = uint64(block.timestamp + 1 hours);
        usdc = new TestToken();
        weth = new TestToken();
        transmitter = new TransmitterMock(IERC20(address(usdc)));
        dex = new DexMock();
        executor = new SwapicExecutor(address(usdc), address(transmitter), address(0), address(this), address(0x6A));
        executor.setKeeper(keeper, true);
        executor.setAllowedTarget(address(dex), true);
        usdc.transfer(address(transmitter), 1e24);
        usdc.transfer(ecoExecutor, 1e24);
        weth.transfer(address(dex), 1e24);
    }

    function _hook(Action action, uint256 minOut) internal view returns (bytes memory) {
        return HookData.encode(
            HookData.Data({
                version: 1,
                ref: keccak256("quote"),
                action: action,
                deadline: deadline,
                recipient: user,
                tokenOut: address(weth),
                minOut: minOut,
                principal: bytes32(0),
                subaccount: bytes32(0)
            })
        );
    }

    function _message(address mintRecipient, uint256 amount, uint256 fee, bytes memory hook)
        internal
        view
        returns (bytes memory)
    {
        bytes32 me = bytes32(uint256(uint160(address(executor))));
        bytes memory header = abi.encodePacked(
            uint32(1), uint32(6), uint32(3), keccak256("nonce"), bytes32(0), bytes32(0), me, uint32(1000), uint32(1000)
        );
        bytes memory body = abi.encodePacked(
            uint32(1),
            bytes32(uint256(uint160(0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913))),
            bytes32(uint256(uint160(mintRecipient))),
            amount,
            bytes32(0),
            fee,
            fee,
            uint256(0),
            hook
        );
        return bytes.concat(header, body);
    }

    function _swap(uint256 amountIn, uint256 out) internal view returns (DexCall memory) {
        return DexCall(address(dex), abi.encodeCall(DexMock.swap, (address(usdc), address(weth), amountIn, out)));
    }

    function test_keeper_completes_the_swap_on_the_minted_amount() public {
        bytes memory m = _message(address(executor), 100e6, 1e4, _hook(Action.Swap, 5e16));
        vm.prank(keeper);
        executor.completeCctp(m, "", _swap(100e6 - 1e4, 6e16));
        assertEq(weth.balanceOf(user), 6e16, "the user got all of the output");
        assertEq(usdc.balanceOf(address(executor)), 0, "nothing held");
        assertEq(weth.balanceOf(address(executor)), 0);
    }

    function test_a_stranger_cannot_choose_the_swap() public {
        bytes memory m = _message(address(executor), 100e6, 0, _hook(Action.Swap, 5e16));
        vm.prank(stranger);
        vm.expectRevert(ISwapicExecutor.NotKeeperOrRecipient.selector);
        executor.completeCctp(m, "", _swap(100e6, 6e16));
    }

    function test_a_short_swap_falls_back_to_the_stable() public {
        bytes memory m = _message(address(executor), 100e6, 0, _hook(Action.Swap, 7e16));
        vm.prank(keeper);
        executor.completeCctp(m, "", _swap(100e6, 6e16));
        assertEq(usdc.balanceOf(user), 100e6, "the minted stable went to the user");
        assertEq(weth.balanceOf(user), 0);
        assertEq(usdc.balanceOf(address(executor)), 0);
    }

    function test_release_pays_the_stable_after_the_deadline_only() public {
        bytes memory m = _message(address(executor), 100e6, 0, _hook(Action.Swap, 5e16));
        vm.prank(stranger);
        vm.expectRevert(ISwapicExecutor.DeadlineNotPassed.selector);
        executor.release(m, "");
        vm.warp(deadline + 1);
        vm.prank(keeper);
        vm.expectRevert(ISwapicExecutor.DeadlinePassed.selector);
        executor.completeCctp(m, "", _swap(100e6, 6e16));
        vm.prank(stranger);
        executor.release(m, "");
        assertEq(usdc.balanceOf(user), 100e6);
    }

    function test_a_message_minted_elsewhere_is_refused() public {
        bytes memory m = _message(stranger, 100e6, 0, _hook(Action.Swap, 5e16));
        vm.prank(keeper);
        vm.expectRevert(ISwapicExecutor.BadMessage.selector);
        executor.completeCctp(m, "", _swap(100e6, 6e16));
    }

    function test_fill_pulls_exactly_the_amount_and_swaps() public {
        vm.startPrank(ecoExecutor);
        usdc.approve(address(executor), 50e6);
        executor.fill(50e6, _hook(Action.Swap, 1e16), _swap(50e6, 2e16));
        vm.stopPrank();
        assertEq(weth.balanceOf(user), 2e16);
        assertEq(usdc.allowance(ecoExecutor, address(executor)), 0, "nothing left approved in Eco's executor");
        assertEq(usdc.balanceOf(address(executor)), 0);
    }

    function test_fill_never_reverts_after_the_funds_arrive() public {
        DexCall memory broken = DexCall(address(dex), hex"deadbeef");
        vm.startPrank(ecoExecutor);
        usdc.approve(address(executor), 50e6);
        executor.fill(50e6, _hook(Action.Swap, 1e16), broken);
        vm.stopPrank();
        assertEq(usdc.balanceOf(user), 50e6, "the stable instead");
    }

    function test_ck_deposit_without_a_helper_falls_back() public {
        bytes memory hook = _hook(Action.CkDeposit, 0);
        vm.startPrank(ecoExecutor);
        usdc.approve(address(executor), 50e6);
        executor.fill(50e6, hook, DexCall(address(0), ""));
        vm.stopPrank();
        assertEq(usdc.balanceOf(user), 50e6);
    }
}
