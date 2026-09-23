// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "forge-std/Test.sol";
import "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import "../src/Vault.sol";
import "./mocks/TestToken.sol";
import "./mocks/SwapRouterMock.sol";
import "./mocks/PaysTheVaultMock.sol";

/// The public door's own property, proved against a target that is allowlisted
/// on the public tier. The tier is a second, independent defence (see
/// VaultAllowlistTiers.t.sol): these tests deliberately give the drain target
/// the widest possible standing, so what refuses it here can only be the door's
/// own rule that a caller's deposit must fund and pay for everything it runs.
contract VaultPublicDrainTest is Test {
    Vault vault;
    TestToken usdc;
    TestToken escrowed;
    SwapRouterMock router;
    PaysTheVaultMock payer;

    address canister = address(0xCA);
    address guardian = address(0x6A);
    address attacker = address(0xBAD);
    address user = address(0xB0B);

    uint256 constant ESCROW = 1000e18;

    function setUp() public {
        Vault impl = new Vault();
        bytes memory init = abi.encodeCall(Vault.initialize, (canister, guardian));
        vault = Vault(payable(address(new ERC1967Proxy(address(impl), init))));

        usdc = new TestToken();
        escrowed = new TestToken();
        router = new SwapRouterMock();
        payer = new PaysTheVaultMock(IERC20(address(escrowed)), address(vault));

        escrowed.transfer(address(payer), ESCROW);
        escrowed.transfer(address(router), 1e24);
        usdc.transfer(attacker, 10e18);
        usdc.transfer(user, 100e18);

        vm.startPrank(canister);
        vault.setRouterAllowlist(address(payer), true, true);
        vault.setRouterAllowlist(address(router), true, true);
        vm.stopPrank();

        vm.prank(attacker);
        usdc.approve(address(vault), type(uint256).max);
        vm.prank(user);
        usdc.approve(address(vault), type(uint256).max);
    }

    function _release(uint256 approveAmount) internal view returns (Vault.Call[] memory calls) {
        calls = new Vault.Call[](1);
        calls[0] = Vault.Call(
            address(payer),
            0,
            abi.encodeCall(PaysTheVaultMock.releaseToTheVault, (ESCROW)),
            address(usdc),
            approveAmount
        );
    }

    function _assertNothingMoved() internal view {
        assertEq(escrowed.balanceOf(attacker), 0, "attacker took escrowed funds");
        assertEq(escrowed.balanceOf(address(payer)), ESCROW, "the escrow was released");
        assertEq(escrowed.balanceOf(address(vault)), 0, "the vault took the release");
    }

    /// The drain, as the research found it: deposit nothing, approve nothing, and
    /// name a payout token the deposit has nothing to do with. Every guard the
    /// door had was satisfied, because none of them said the caller had to pay.
    function test_zero_deposit_with_a_free_call_is_refused() public {
        vm.prank(attacker);
        vm.expectRevert(Vault.EmptyDeposit.selector);
        vault.depositAndExecute("anything", address(usdc), 0, _release(0), address(escrowed), 0, attacker);
        _assertNothingMoved();
    }

    /// Raising the bar to a non-zero deposit alone would not have closed it: the
    /// attacker pays a dust deposit and keeps the same free call.
    function test_a_call_that_approves_nothing_is_refused() public {
        vm.prank(attacker);
        vm.expectRevert(Vault.PublicCallNotAllowed.selector);
        vault.depositAndExecute("anything", address(usdc), 1e18, _release(0), address(escrowed), 0, attacker);
        _assertNothingMoved();
    }

    /// And approving dust would not have closed it either: the door requires the
    /// target to actually take what it was approved, which a contract that only
    /// pays the vault never does. This is the line that makes the door's safety a
    /// property rather than a list of forbidden shapes.
    function test_a_target_that_does_not_take_the_deposit_is_refused() public {
        vm.prank(attacker);
        vm.expectRevert(Vault.DepositNotSpent.selector);
        vault.depositAndExecute("anything", address(usdc), 1e18, _release(1e18), address(escrowed), 0, attacker);
        _assertNothingMoved();
        assertEq(usdc.balanceOf(attacker), 10e18, "the dust deposit rolled back");
    }

    /// Nor can the free call ride along beside a real swap that does spend the
    /// deposit: the approvals may sum to at most the deposit, and every one of
    /// them has to be taken, so there is nothing left to fund a second target.
    function test_a_real_swap_cannot_carry_a_free_call_alongside_it() public {
        Vault.Call[] memory calls = new Vault.Call[](2);
        calls[0] = Vault.Call(
            address(router),
            0,
            abi.encodeCall(SwapRouterMock.swapAtoB, (address(usdc), address(escrowed), 1e18, 1)),
            address(usdc),
            1e18
        );
        calls[1] = _release(1)[0];

        // the approvals now sum to 1e18 + 1, which is more than the 1e18 deposited
        vm.prank(attacker);
        vm.expectRevert(Vault.PublicCallNotAllowed.selector);
        vault.depositAndExecute("anything", address(usdc), 1e18, calls, address(escrowed), 0, attacker);
        _assertNothingMoved();

        // and funding both out of one deposit leaves the free call's share untaken
        calls[0].approveAmount = 1e18 - 1;
        calls[0].data = abi.encodeCall(SwapRouterMock.swapAtoB, (address(usdc), address(escrowed), 1e18 - 1, 1));
        vm.prank(attacker);
        vm.expectRevert(Vault.DepositNotSpent.selector);
        vault.depositAndExecute("anything", address(usdc), 1e18, calls, address(escrowed), 0, attacker);
        _assertNothingMoved();
    }

    /// The positive control: the door still does the job it exists for, so the
    /// refusals above cannot be passing because the door stopped working.
    function test_an_honest_swap_still_goes_through_the_door() public {
        Vault.Call[] memory calls = new Vault.Call[](1);
        calls[0] = Vault.Call(
            address(router),
            0,
            abi.encodeCall(SwapRouterMock.swapAtoB, (address(usdc), address(escrowed), 100e18, 95e18)),
            address(usdc),
            100e18
        );

        vm.prank(user);
        vault.depositAndExecute("honest", address(usdc), 100e18, calls, address(escrowed), 90e18, user);

        assertEq(escrowed.balanceOf(user), 95e18, "the swap paid out");
        assertEq(usdc.balanceOf(user), 0, "the deposit was spent");
        assertEq(usdc.allowance(address(vault), address(router)), 0, "no approval left standing");
    }
}
