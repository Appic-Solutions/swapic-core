# swapic-core

On-chain core of Swapic: the Vault contract (per-EVM-chain custody and execution)
and the settlement canister.

## Layout

- `contracts/`: the Vault, a Foundry project.
- `backend/canisters/settlement/api`: the canister's candid types, per-endpoint
  `Args`/`Response` aliases, `can.did`, and the golden vectors in `golden/`.
- `backend/canisters/settlement/impl`: the canister itself (`lifecycle`, `queries`,
  `updates`, `guards`, `state`, `storage`, `task_manager`).
- `backend/integration_tests`: the pocket-ic suite, driven through a typed client.
- `backend/libraries/types`: the domain types: checked amounts, identifiers, and the
  rules each value must satisfy.
- `scripts/`: `build-canister.sh`, `generate-did.sh`, `run-integration-tests.sh`.
