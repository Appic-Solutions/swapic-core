# swapic-core

On-chain core of Swapic: the Vault contract (per-EVM-chain custody and execution)
and the settlement canister.

## Layout

- `contracts/`: the Vault, a Foundry project.
- `backend/canisters/settlement/api`: the canister's candid types, per-endpoint
  `Args`/`Response` aliases, `can.did`, and the conversions between the two.
- `backend/canisters/settlement/impl`: the canister itself (`lifecycle`, `queries`,
  `updates`, `guards`, `state`, `storage`, `task_manager`).
- `backend/integration_tests`: the pocket-ic suite, driven through a typed client.
- `backend/libraries/types`: the domain types: checked amounts, identifiers, the rules
  each value must satisfy, the canonical quote and event codecs, and their golden vectors
  in `golden/`.
- `scripts/`: `build-canister.sh`, `generate-did.sh`, `run-integration-tests.sh`.
