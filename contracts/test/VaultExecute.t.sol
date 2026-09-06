// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "forge-std/Test.sol";
import "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import "../src/Vault.sol";
import "./mocks/TestToken.sol";
import "./mocks/SwapRouterMock.sol";
import "./mocks/EvilRouterMock.sol";

contract VaultExecuteTest is Test {
    Vault vault;
    TestToken a;
    TestToken b;
    SwapRouterMock router;
    EvilRouterMock evilRouter;
    address canister = address(0xCA);
    address guardian = address(0x6A);

    function setUp() public {
        Vault impl = new Vault();
        bytes memory init = abi.encodeCall(Vault.initialize, (canister, guardian));
        vault = Vault(payable(address(new ERC1967Proxy(address(impl), init))));

        a = new TestToken();
        b = new TestToken();
        router = new SwapRouterMock();
        evilRouter = new EvilRouterMock();

        a.transfer(address(this), 100e18);
        b.transfer(address(this), 100e18);

        a.transfer(address(vault), 100e18);
        b.transfer(address(router), 95e18);

        vm.prank(canister);
        vault.setRouterAllowlist(address(router), true);
        vm.prank(canister);
        vault.setRouterAllowlist(address(evilRouter), true);
    }

    function test_execute_happy_path_checks_delta_and_zeroes_approval() public {
        Vault.Call[] memory calls = new Vault.Call[](1);
        calls[0] = Vault.Call(
            address(router),
            0,
            abi.encodeCall(SwapRouterMock.swapAtoB, (address(a), address(b), 100e18, 95e18)),
            address(a),
            100e18
        );
        Vault.Delta[] memory deltas = new Vault.Delta[](2);
        deltas[0] = Vault.Delta(address(a), -int256(100e18));
        deltas[1] = Vault.Delta(address(b), int256(90e18));
        vm.prank(canister);
        vault.execute("s1", calls, deltas);
        assertEq(a.allowance(address(vault), address(router)), 0, "approval zeroed");
        assertEq(b.balanceOf(address(vault)), 95e18);
    }

    function test_execute_rejects_unallowlisted_target() public {
        Vault.Call[] memory calls = new Vault.Call[](1);
        calls[0] = Vault.Call(address(0xBAD), 0, "", address(0), 0);
        vm.prank(canister);
        vm.expectRevert(Vault.TargetNotAllowed.selector);
        vault.execute("s2", calls, new Vault.Delta[](0));
    }

    function test_execute_reverts_when_delta_missed() public {
        Vault.Call[] memory evilCalls = new Vault.Call[](1);
        evilCalls[0] = Vault.Call(
            address(evilRouter),
            0,
            abi.encodeCall(EvilRouterMock.swapAtoB, (address(a), address(b), 100e18, 0)),
            address(a),
            100e18
        );
        Vault.Delta[] memory deltasExpectingB = new Vault.Delta[](1);
        deltasExpectingB[0] = Vault.Delta(address(b), int256(90e18));
        vm.prank(canister);
        vm.expectRevert(Vault.DeltaMissed.selector);
        vault.execute("s3", evilCalls, deltasExpectingB);
        assertEq(a.allowance(address(vault), address(evilRouter)), 0);
        assertEq(a.balanceOf(address(vault)), 100e18);
    }

    function test_execute_only_canister() public {
        vm.expectRevert(Vault.OnlyCanister.selector);
        vault.execute("s4", new Vault.Call[](0), new Vault.Delta[](0));
    }
}
