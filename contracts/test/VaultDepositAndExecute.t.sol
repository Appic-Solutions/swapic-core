// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "forge-std/Test.sol";
import "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import "../src/Vault.sol";
import "./mocks/TestToken.sol";
import "./mocks/SwapRouterMock.sol";

contract VaultDepositAndExecuteTest is Test {
    Vault vault;
    TestToken a;
    TestToken b;
    SwapRouterMock router;
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
        router = new SwapRouterMock();

        a.transfer(user, 100e18);
        a.transfer(other, 100e18);
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
        emit Vault.Deposited("q1", address(a), user, 100e18);
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
        bool sawDeposited;
        bool sawExecuted;
        bool sawPayout;
        for (uint256 i = 0; i < logs.length; i++) {
            if (logs[i].emitter != address(vault)) continue;
            bytes32 t = logs[i].topics[0];
            if (t == keccak256("Deposited(bytes32,address,address,uint256)")) sawDeposited = true;
            if (t == keccak256("Executed(bytes32)")) sawExecuted = true;
            if (t == keccak256("Payout(bytes32,address,address,uint256)")) sawPayout = true;
        }
        // the positive assertions prove the scan works, so the negative one cannot pass vacuously
        assertTrue(sawDeposited, "Deposited emitted");
        assertTrue(sawExecuted, "Executed emitted");
        assertFalse(sawPayout, "no Payout without a payoutTo");
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
