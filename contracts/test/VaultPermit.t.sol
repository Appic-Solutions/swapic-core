// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "forge-std/Test.sol";
import "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import "../src/Vault.sol";
import "./mocks/PermitToken.sol";

contract VaultPermitTest is Test {
    Vault vault;
    PermitToken permitToken;
    address canister = address(0xCA);
    address guardian = address(0x6A);
    uint256 userKey = 0xA11CE;
    address user = vm.addr(userKey);

    function setUp() public {
        Vault impl = new Vault();
        bytes memory init = abi.encodeCall(Vault.initialize, (canister, guardian));
        vault = Vault(payable(address(new ERC1967Proxy(address(impl), init))));

        permitToken = new PermitToken();
        permitToken.transfer(user, 1000e18);
    }

    function signPermit(uint256 signerKey, address spender, uint256 value, uint256 deadline)
        internal
        view
        returns (uint8 v, bytes32 r, bytes32 s)
    {
        bytes32 structHash = keccak256(
            abi.encode(
                keccak256("Permit(address owner,address spender,uint256 value,uint256 nonce,uint256 deadline)"),
                user,
                spender,
                value,
                permitToken.nonces(user),
                deadline
            )
        );
        bytes32 digest = keccak256(abi.encodePacked("\x19\x01", permitToken.DOMAIN_SEPARATOR(), structHash));
        (v, r, s) = vm.sign(signerKey, digest);
    }

    function test_pull_with_permit_moves_funds_user_pays_nothing() public {
        uint256 deadline = block.timestamp + 1 hours;
        (uint8 v, bytes32 r, bytes32 s) = signPermit(userKey, address(vault), 50e18, deadline);
        vm.prank(canister);
        vault.pullWithPermit("q1", address(permitToken), user, 50e18, deadline, v, r, s);
        assertEq(permitToken.balanceOf(address(vault)), 50e18);
    }

    function test_pull_survives_permit_front_run() public {
        uint256 deadline = block.timestamp + 1 hours;
        (uint8 v, bytes32 r, bytes32 s) = signPermit(userKey, address(vault), 50e18, deadline);
        permitToken.permit(user, address(vault), 50e18, deadline, v, r, s); // griefer consumes it first
        vm.prank(canister);
        vault.pullWithPermit("q2", address(permitToken), user, 50e18, deadline, v, r, s); // must still work
        assertEq(permitToken.balanceOf(address(vault)), 50e18);
    }

    function test_pull_only_canister() public {
        vm.expectRevert(Vault.OnlyCanister.selector);
        vault.pullWithPermit("q3", address(permitToken), user, 1, 0, 0, 0, 0);
    }
}
