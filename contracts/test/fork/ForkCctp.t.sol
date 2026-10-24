// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "./ForkBase.sol";
import "../helpers/CctpPlan.sol";

/// Base: `run` of WETH through a real KyberSwap recipe (fetched with the Runner
/// as recipient), then a `depositForBurnWithHook` step on the real
/// TokenMessengerV2 with the amount written at byte 4.
contract ForkCctpTest is ForkBase {
    bytes32 constant TRANSFER = keccak256("Transfer(address,address,uint256)");

    string j;

    function setUp() public {
        j = _fixture("kyber_base_weth_usdc");
        if (!_fork("base", "BASE_RPC_URL", vm.parseJsonUint(j, ".block"))) return;
        _deploy(USDC_BASE, WETH_BASE);
        deal(WETH_BASE, payer, 5e16);
        vm.prank(payer);
        IERC20(WETH_BASE).approve(SWAPIC, type(uint256).max);
    }

    function test_kyber_swap_then_cctp_burn_of_exactly_the_output() public {
        uint256 quoted = vm.parseJsonUint(j, ".amountOut");
        assertEq(vm.parseJsonUint(j, ".amountIn"), 5e16);
        Order memory o;
        o.ref = keccak256("fork cctp");
        o.payer = payer;
        o.tokenIn = WETH_BASE;
        o.amountIn = 5e16;
        o.deadline = block.timestamp + 60;
        o.steps = new Step[](2);
        o.steps[0] = Step(
            vm.parseJsonAddress(j, ".routerAddress"), vm.parseJsonBytes(j, ".data"), WETH_BASE, 5e16, new Write[](0)
        );
        bytes memory env = _envelope(o.ref, Action.Swap, payer, WETH_ARB, 1);
        o.steps[1] = CctpPlan.burnStep(
            MESSENGER, USDC_BASE, quoted * 98 / 100, 3, RECEIVER, RECEIVER, quoted / 100, 1000, env
        );

        bytes memory cd = abi.encodeCall(Swapic.run, (o));
        uint256 blk = vm.parseJsonUint(j, ".block");
        vm.recordLogs();
        vm.prank(payer);
        bytes32 id = abi.decode(_call("run: KyberSwap WETH leg + CCTP burn", blk, SWAPIC, cd), (bytes32));
        Vm.Log[] memory logs = vm.getRecordedLogs();

        uint256 swapOut;
        uint256 burned;
        bool seen;
        for (uint256 i; i < logs.length; ++i) {
            Vm.Log memory l = logs[i];
            if (l.emitter == USDC_BASE && l.topics[0] == TRANSFER && l.topics[2] == bytes32(uint256(uint160(RUNNER)))) {
                swapOut += abi.decode(l.data, (uint256));
            }
            if (l.emitter == MESSENGER && l.topics[0] == DEPOSIT_FOR_BURN) {
                (uint256 amount, bytes32 mintRecipient, uint32 domain,, bytes32 caller,, bytes memory hook) =
                    abi.decode(l.data, (uint256, bytes32, uint32, bytes32, bytes32, uint256, bytes));
                assertEq(l.topics[1], bytes32(uint256(uint160(USDC_BASE))), "burn token");
                assertEq(l.topics[2], bytes32(uint256(uint160(RUNNER))), "depositor is the Runner");
                assertEq(mintRecipient, bytes32(uint256(uint160(RECEIVER))));
                assertEq(caller, bytes32(uint256(uint160(RECEIVER))));
                assertEq(domain, 3);
                assertEq(hook, env, "the envelope rides as hook data");
                burned = amount;
                seen = true;
            }
        }
        assertTrue(seen, "burned on the real messenger");
        assertGt(swapOut, quoted * 98 / 100);
        assertEq(burned, swapOut, "the burn is exactly the swap output, minus nothing");
        console2.log("block", block.number, "swap out", swapOut);
        assertTrue(swapic.used(id));
        assertEq(IERC20(WETH_BASE).balanceOf(payer), 0);
        _assertRunnerEmpty(WETH_BASE, USDC_BASE);
    }

    /// Ether in: Swapic wraps it at the door, the same WETH recipe runs.
    function test_native_in_is_wrapped_at_the_door() public {
        uint256 quoted = vm.parseJsonUint(j, ".amountOut");
        Order memory o;
        o.ref = keccak256("fork cctp native");
        o.payer = payer;
        o.tokenIn = NATIVE;
        o.amountIn = 5e16;
        o.deadline = block.timestamp + 60;
        o.steps = new Step[](2);
        o.steps[0] = Step(
            vm.parseJsonAddress(j, ".routerAddress"), vm.parseJsonBytes(j, ".data"), WETH_BASE, 5e16, new Write[](0)
        );
        o.steps[1] = CctpPlan.burnStep(
            MESSENGER, USDC_BASE, quoted * 98 / 100, 3, RECEIVER, RECEIVER, quoted / 100, 1000, _envelope(o.ref, Action.Swap, payer, WETH_ARB, 1)
        );
        vm.deal(payer, 5e16);
        vm.recordLogs();
        vm.prank(payer);
        swapic.run{value: 5e16}(o);
        Vm.Log[] memory logs = vm.getRecordedLogs();
        uint256 burned;
        for (uint256 i; i < logs.length; ++i) {
            if (logs[i].emitter == MESSENGER && logs[i].topics[0] == DEPOSIT_FOR_BURN) {
                (burned) = abi.decode(logs[i].data, (uint256));
            }
        }
        assertGt(burned, quoted * 98 / 100, "the wrapped ether was swapped and burned");
        assertEq(payer.balance, 0);
        _assertRunnerEmpty(WETH_BASE, USDC_BASE);
    }
}

