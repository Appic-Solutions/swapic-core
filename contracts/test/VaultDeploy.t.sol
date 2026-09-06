// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "forge-std/Test.sol";
import "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import "../script/DeployVault.s.sol";
import "../src/Vault.sol";

contract VaultDeployTest is Test {
    DeployVault script;

    bytes32 constant SALT = keccak256("swapic.vault.v1");
    address constant CANISTER = address(0xCA);
    address constant GUARDIAN = address(0x6A);

    function setUp() public {
        // one instance for the whole test: in `forge test` the CREATE2 deployer is
        // the contract running `new X{salt: ...}`, so a second instance would be a
        // different deployer and would legitimately produce different addresses
        script = new DeployVault();
    }

    function _proxyInitCodeHash(address impl, bytes memory initData) internal pure returns (bytes32) {
        return keccak256(abi.encodePacked(type(ERC1967Proxy).creationCode, abi.encode(impl, initData)));
    }

    function test_addresses_are_deterministic_across_chains() public {
        bytes memory initData = abi.encodeCall(Vault.initialize, (CANISTER, GUARDIAN));
        address expectedImpl = vm.computeCreate2Address(SALT, keccak256(type(Vault).creationCode), address(script));
        address expectedProxy =
            vm.computeCreate2Address(SALT, _proxyInitCodeHash(expectedImpl, initData), address(script));

        uint256 snap = vm.snapshotState();
        (address impl1, address proxy1) = script.deploy(SALT, CANISTER, GUARDIAN);
        assertEq(impl1, expectedImpl, "impl address is not the CREATE2 preimage");
        assertEq(proxy1, expectedProxy, "proxy address is not the CREATE2 preimage");

        // same deployer, same salt, same init code, different chain: same address
        vm.revertToState(snap);
        vm.chainId(999);
        (address impl2, address proxy2) = script.deploy(SALT, CANISTER, GUARDIAN);
        assertEq(impl2, impl1, "impl address drifted between chains");
        assertEq(proxy2, proxy1, "proxy address drifted between chains");
    }

    function test_proxy_is_initialized_in_its_own_constructor() public {
        (address impl, address proxy) = script.deploy(SALT, CANISTER, GUARDIAN);
        Vault vault = Vault(payable(proxy));
        assertEq(vault.canister(), CANISTER, "canister not set at construction");
        assertEq(vault.guardian(), GUARDIAN, "guardian not set at construction");

        vm.expectRevert();
        vault.initialize(address(1), address(2));

        // the init calldata is a constructor argument, so it is inside the init code
        // hash: roles other than ours could only ever land at a different address
        bytes memory hostileInit = abi.encodeCall(Vault.initialize, (address(0xBAD), address(0xBAD)));
        assertTrue(
            vm.computeCreate2Address(SALT, _proxyInitCodeHash(impl, hostileInit), address(script)) != proxy,
            "different roles must not share the address"
        );
    }

    function test_different_salt_gives_a_different_address() public {
        (, address proxyA) = script.deploy(bytes32(uint256(1)), CANISTER, GUARDIAN);
        (, address proxyB) = script.deploy(bytes32(uint256(2)), CANISTER, GUARDIAN);
        assertTrue(proxyA != proxyB, "salt must move the address");
    }
}
