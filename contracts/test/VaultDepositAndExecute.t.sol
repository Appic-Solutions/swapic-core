// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "forge-std/Test.sol";
import "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import "../src/Vault.sol";
import "./mocks/TestToken.sol";
import "./mocks/SwapRouterMock.sol";
import "./mocks/NativeRouterMock.sol";
import "./mocks/FeeOnTransferToken.sol";

contract VaultDepositAndExecuteTest is Test {
    Vault vault;
    TestToken a;
    TestToken b;
    FeeOnTransferToken feeToken;
    SwapRouterMock router;
    NativeRouterMock nativeRouter;
    address canister = address(0xCA);
    address guardian = address(0x6A);
    address user = address(0xB0B);
    address other = address(0xA11CE);

    function setUp() public {
        Vault impl = new Vault();
        bytes memory init = abi.encodeCall(Vault.initialize, (canister, guardian));
        vault = Vault(payable(address(new ERC1967Proxy(address(impl), init))));

        a = new TestToken();
        b = new TestToken();
        feeToken = new FeeOnTransferToken();
        router = new SwapRouterMock();
        nativeRouter = new NativeRouterMock();
        vm.deal(address(nativeRouter), 10 ether);

        a.transfer(user, 100e18);
        a.transfer(other, 100e18);
        feeToken.transfer(user, 200e18);
        b.transfer(address(router), 500e18);

        vm.prank(canister);
        vault.setRouterAllowlist(address(router), true);
    }

    function _swapCalls(uint256 amountIn, uint256 amountOut) internal view returns (Vault.Call[] memory calls) {
        calls = new Vault.Call[](1);
        calls[0] = Vault.Call(
            address(router),
            0,
            abi.encodeCall(SwapRouterMock.swapAtoB, (address(a), address(b), amountIn, amountOut)),
            address(a),
            amountIn
        );
    }

    function test_deposit_and_execute_pays_user_and_emits_all_three() public {
        vm.startPrank(user);
        a.approve(address(vault), 100e18);

        vm.expectEmit(true, true, true, true);
        emit Vault.AtomicSwap("q1", address(a), user, 100e18);
        vm.expectEmit(true, true, true, true);
        emit Vault.Executed("q1");
        vm.expectEmit(true, true, true, true);
        emit Vault.Payout("q1", address(b), user, 95e18);

        vault.depositAndExecute("q1", address(a), 100e18, _swapCalls(100e18, 95e18), address(b), 90e18, user);
        vm.stopPrank();

        assertEq(b.balanceOf(user), 95e18, "user paid out");
        assertEq(a.balanceOf(user), 0, "deposit pulled");
        assertEq(a.allowance(address(vault), address(router)), 0, "approval zeroed");
        assertEq(b.balanceOf(address(vault)), 0, "nothing stranded in the vault");
        assertTrue(vault.quoteKeyUsed("q1", user), "quote marked for the payer");
    }

    function test_min_out_miss_reverts_everything() public {
        vm.startPrank(user);
        a.approve(address(vault), 100e18);
        vm.expectRevert(Vault.DeltaMissed.selector);
        vault.depositAndExecute("q2", address(a), 100e18, _swapCalls(100e18, 50e18), address(b), 90e18, user);
        vm.stopPrank();

        assertEq(a.balanceOf(user), 100e18, "user keeps their A");
        assertEq(a.balanceOf(address(vault)), 0, "vault holds no A");
        assertEq(b.balanceOf(address(vault)), 0, "vault holds no B");
        assertEq(b.balanceOf(user), 0, "user got no B");
        assertFalse(vault.quoteKeyUsed("q2", user), "quote not burned");
    }

    function test_zero_payout_to_keeps_proceeds_in_vault() public {
        vm.startPrank(user);
        a.approve(address(vault), 100e18);
        vm.recordLogs();
        vault.depositAndExecute("q3", address(a), 100e18, _swapCalls(100e18, 95e18), address(b), 90e18, address(0));
        vm.stopPrank();

        assertEq(b.balanceOf(address(vault)), 95e18, "proceeds stay in the vault");
        assertEq(b.balanceOf(user), 0, "user paid nothing out");

        Vm.Log[] memory logs = vm.getRecordedLogs();
        bool sawAtomicSwap;
        bool sawExecuted;
        bool sawPayout;
        bool sawDeposited;
        for (uint256 i = 0; i < logs.length; i++) {
            if (logs[i].emitter != address(vault)) continue;
            bytes32 t = logs[i].topics[0];
            if (t == keccak256("AtomicSwap(bytes32,address,address,uint256)")) sawAtomicSwap = true;
            if (t == keccak256("Executed(bytes32)")) sawExecuted = true;
            if (t == keccak256("Payout(bytes32,address,address,uint256)")) sawPayout = true;
            if (t == keccak256("Deposited(bytes32,address,address,uint256)")) sawDeposited = true;
        }
        // the positive assertions prove the scan works, so the negative ones cannot pass vacuously
        assertTrue(sawAtomicSwap, "AtomicSwap emitted");
        assertTrue(sawExecuted, "Executed emitted");
        assertFalse(sawPayout, "no Payout without a payoutTo");
        // never Deposited: the canister credits cross-chain swaps off that log
        assertFalse(sawDeposited, "atomic path must not look like a cross-chain deposit");
    }

    function test_blocked_when_deposits_paused() public {
        vm.prank(guardian);
        vault.pause(Vault.PauseClass.Deposits);

        vm.startPrank(user);
        a.approve(address(vault), 100e18);
        vm.expectRevert(Vault.IsPaused.selector);
        vault.depositAndExecute("q4", address(a), 100e18, _swapCalls(100e18, 95e18), address(b), 90e18, user);
        vm.stopPrank();
    }

    function test_duplicate_quote_hash_by_same_payer_reverts() public {
        vm.startPrank(user);
        a.approve(address(vault), 100e18);
        vault.depositAndExecute("q5", address(a), 50e18, _swapCalls(50e18, 50e18), address(b), 50e18, user);
        vm.expectRevert(Vault.QuoteHashUsed.selector);
        vault.depositAndExecute("q5", address(a), 50e18, _swapCalls(50e18, 50e18), address(b), 50e18, user);
        vm.stopPrank();
    }

    function test_deposit_itself_never_counts_toward_min_out() public {
        // token == payoutToken with no calls: the payoutToken snapshot is taken after
        // the pull, so the deposit cannot satisfy minOut on its own
        vm.startPrank(user);
        a.approve(address(vault), 10e18);
        vm.expectRevert(Vault.DeltaMissed.selector);
        vault.depositAndExecute("q11", address(a), 10e18, new Vault.Call[](0), address(a), 1, user);
        vm.stopPrank();

        assertEq(a.balanceOf(user), 100e18, "rolled back");
    }

    function test_approval_sum_across_calls_rejected() public {
        // each call is individually under the received deposit; only the sum is over
        Vault.Call[] memory calls = new Vault.Call[](2);
        calls[0] = Vault.Call(
            address(router),
            0,
            abi.encodeCall(SwapRouterMock.swapAtoB, (address(a), address(b), 60e18, 55e18)),
            address(a),
            60e18
        );
        calls[1] = calls[0];

        vm.startPrank(user);
        a.approve(address(vault), 100e18);
        vm.expectRevert(Vault.PublicCallNotAllowed.selector);
        vault.depositAndExecute("q12", address(a), 100e18, calls, address(b), 90e18, user);
        vm.stopPrank();

        assertEq(a.balanceOf(user), 100e18, "user keeps their A");
    }

    function test_stranger_cannot_spend_the_vaults_pooled_funds() public {
        // the vault pools B between operations; an attacker depositing dust of A
        // must not be able to route that B out through an allowlisted router
        b.transfer(address(vault), 100e18);
        address attacker = address(0xBAD);
        a.transfer(attacker, 1e18);

        Vault.Call[] memory calls = new Vault.Call[](1);
        calls[0] = Vault.Call(
            address(router),
            0,
            abi.encodeCall(SwapRouterMock.swapAtoB, (address(b), address(a), 100e18, 1)),
            address(b),
            100e18
        );

        vm.startPrank(attacker);
        a.approve(address(vault), 1e18);
        vm.expectRevert(Vault.PublicCallNotAllowed.selector);
        vault.depositAndExecute("evil", address(a), 1e18, calls, address(a), 0, attacker);
        vm.stopPrank();

        assertEq(b.balanceOf(address(vault)), 100e18, "pooled B untouched");
        assertEq(a.balanceOf(attacker), 1e18, "attacker gained nothing");
        assertEq(b.allowance(address(vault), address(router)), 0, "no approval leaked");
    }

    function test_call_with_native_value_rejected() public {
        // approveToken and approveAmount are both valid here, so `value` is the sole violation
        Vault.Call[] memory calls = new Vault.Call[](1);
        calls[0] = Vault.Call(
            address(router),
            1,
            abi.encodeCall(SwapRouterMock.swapAtoB, (address(a), address(b), 100e18, 95e18)),
            address(a),
            100e18
        );

        vm.startPrank(user);
        a.approve(address(vault), 100e18);
        vm.expectRevert(Vault.PublicCallNotAllowed.selector);
        vault.depositAndExecute("q7", address(a), 100e18, calls, address(b), 90e18, user);
        vm.stopPrank();

        assertEq(a.balanceOf(user), 100e18, "user keeps their A");
    }

    function test_approval_above_received_deposit_rejected() public {
        // the fee-on-transfer token delivers 99e18 of a 100e18 deposit, so approving
        // the requested amount exceeds what the caller actually paid in
        Vault.Call[] memory calls = new Vault.Call[](1);
        calls[0] = Vault.Call(
            address(router),
            0,
            abi.encodeCall(SwapRouterMock.swapAtoB, (address(feeToken), address(b), 100e18, 95e18)),
            address(feeToken),
            100e18
        );

        vm.startPrank(user);
        feeToken.approve(address(vault), 100e18);
        vm.expectRevert(Vault.PublicCallNotAllowed.selector);
        vault.depositAndExecute("q8", address(feeToken), 100e18, calls, address(b), 90e18, user);
        vm.stopPrank();

        assertEq(feeToken.balanceOf(address(vault)), 0, "deposit rolled back");
    }

    function test_payouts_pause_gates_the_payout_but_not_vault_retention() public {
        vm.prank(guardian);
        vault.pause(Vault.PauseClass.Payouts);

        vm.startPrank(user);
        a.approve(address(vault), 100e18);
        vm.expectRevert(Vault.IsPaused.selector);
        vault.depositAndExecute("q9", address(a), 100e18, _swapCalls(100e18, 95e18), address(b), 90e18, user);

        // proceeds staying in the vault are Deposits-gated only, so this still works
        vault.depositAndExecute("q10", address(a), 100e18, _swapCalls(100e18, 95e18), address(b), 90e18, address(0));
        vm.stopPrank();

        assertEq(b.balanceOf(address(vault)), 95e18, "proceeds retained");
        assertEq(b.balanceOf(user), 0, "nothing paid out while Payouts is paused");
    }

    function test_native_payout_leg_goes_through_the_send_door() public {
        // a direct safeTransfer here reverts SafeERC20FailedOperation on address(0),
        // so this pins the atomic path's native payout to _send
        address eoa = makeAddr("nativeReceiver");
        vm.prank(canister);
        vault.setRouterAllowlist(address(nativeRouter), true);

        Vault.Call[] memory calls = new Vault.Call[](1);
        calls[0] = Vault.Call(
            address(nativeRouter),
            0,
            abi.encodeCall(NativeRouterMock.swapForNative, (address(a), 100e18, 1 ether)),
            address(a),
            100e18
        );

        vm.startPrank(user);
        a.approve(address(vault), 100e18);
        vm.expectEmit(true, true, true, true);
        emit Vault.Payout("qn", address(0), eoa, 1 ether);
        vault.depositAndExecute("qn", address(a), 100e18, calls, address(0), 1 ether, eoa);
        vm.stopPrank();

        assertEq(eoa.balance, 1 ether, "native proceeds paid out");
        assertEq(address(vault).balance, 0, "no native stranded in the vault");
        assertEq(a.balanceOf(user), 0, "deposit pulled");
    }

    function test_different_payer_may_reuse_the_same_quote_hash() public {
        vm.startPrank(user);
        a.approve(address(vault), 100e18);
        vault.depositAndExecute("q6", address(a), 100e18, _swapCalls(100e18, 95e18), address(b), 90e18, user);
        vm.stopPrank();

        vm.startPrank(other);
        a.approve(address(vault), 100e18);
        vault.depositAndExecute("q6", address(a), 100e18, _swapCalls(100e18, 95e18), address(b), 90e18, other);
        vm.stopPrank();

        assertEq(b.balanceOf(user), 95e18, "first payer paid");
        assertEq(b.balanceOf(other), 95e18, "second payer paid on the same hash");
    }
}
