# Swapic Vault (contracts)

Foundry project for the Swapic Vault, the EVM-side custody contract: the settlement canister is its sole operator, a guardian may pause but never move funds, and every exit goes through one send door.

- Build: `forge build`
- Test: `forge test`. Two Permit2 tests fork Base and skip themselves unless `BASE_RPC_URL` is set.
- Deploy: `script/DeployVault.s.sol` is the deterministic CREATE2 deploy, run only as part of the deploy ceremony.
