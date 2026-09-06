// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "forge-std/Test.sol";
import "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import "../src/Vault.sol";

contract VaultInitTest is Test {
    Vault vault;
    address canister = address(0xCA);
    address guardian = address(0x6A);

    function setUp() public {
        Vault impl = new Vault();
        bytes memory init = abi.encodeCall(Vault.initialize, (canister, guardian));
        vault = Vault(payable(address(new ERC1967Proxy(address(impl), init))));
    }

    function test_roles_are_set() public view {
        assertEq(vault.canister(), canister);
        assertEq(vault.guardian(), guardian);
    }

    function test_cannot_initialize_twice() public {
        vm.expectRevert();
        vault.initialize(address(1), address(2));
    }

    function test_only_canister_can_upgrade() public {
        Vault newImpl = new Vault();
        vm.expectRevert(Vault.OnlyCanister.selector);
        vault.upgradeToAndCall(address(newImpl), "");
        vm.prank(canister);
        vault.upgradeToAndCall(address(newImpl), "");
    }
}
