// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "forge-std/Test.sol";
import "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import "../src/Vault.sol";

contract VaultPauseTest is Test {
    Vault vault;
    address canister = address(0xCA);
    address guardian = address(0x6A);

    function setUp() public {
        Vault impl = new Vault();
        bytes memory init = abi.encodeCall(Vault.initialize, (canister, guardian));
        vault = Vault(payable(address(new ERC1967Proxy(address(impl), init))));
    }

    function test_guardian_can_pause_but_not_unpause() public {
        vm.prank(guardian);
        vault.pause(Vault.PauseClass.Deposits);
        assertTrue(vault.paused(Vault.PauseClass.Deposits));
        vm.prank(guardian);
        vm.expectRevert(Vault.OnlyCanister.selector);
        vault.unpause(Vault.PauseClass.Deposits);
    }

    function test_canister_can_pause_and_unpause() public {
        vm.startPrank(canister);
        vault.pause(Vault.PauseClass.Payouts);
        vault.unpause(Vault.PauseClass.Payouts);
        vm.stopPrank();
        assertFalse(vault.paused(Vault.PauseClass.Payouts));
    }

    function test_random_address_cannot_pause() public {
        vm.expectRevert(Vault.OnlyGuardianOrCanister.selector);
        vault.pause(Vault.PauseClass.Deposits);
    }
}
