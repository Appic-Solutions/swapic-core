// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "forge-std/Script.sol";
import "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import "../src/Vault.sol";

contract DeployVault is Script {
    /// CREATE2 both legs, and hand the proxy its `initialize` calldata as a constructor
    /// argument so the roles are set inside the proxy's own creation: there is never an
    /// uninitialized proxy on chain for anyone to front-run, and because the calldata is
    /// part of the init code hash, different roles could only land at a different address.
    ///
    /// Determinism: `new X{salt: s}` is a CREATE2 by whatever contract runs it, so in
    /// `forge test` the deployer is this script instance. Under `vm.startBroadcast`,
    /// forge instead routes CREATE2 through the canonical deterministic-deployment proxy
    /// (0x4e59b44847b379578588920cA78FbF26c0B4956C), which is what makes the address the
    /// same on every chain for the same salt and init code.
    function deploy(bytes32 salt, address canister, address guardian) public returns (address impl, address proxy) {
        impl = address(new Vault{salt: salt}());
        bytes memory initData = abi.encodeCall(Vault.initialize, (canister, guardian));
        proxy = address(new ERC1967Proxy{salt: salt}(impl, initData));
    }

    function run() external returns (address impl, address proxy) {
        bytes32 salt = vm.envBytes32("VAULT_SALT");
        address canister = vm.envAddress("CANISTER_ADDRESS");
        address guardian = vm.envAddress("GUARDIAN_ADDRESS");

        vm.startBroadcast();
        (impl, proxy) = deploy(salt, canister, guardian);
        vm.stopBroadcast();
    }
}
