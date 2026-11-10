# swapic-core

The core of Swapic: the settlement canister on the Internet Computer and the contracts on
each EVM chain.

## What runs in prod

- The canister `settlement` is `x4cav-kaaaa-aaaar-qciya-cai` (see `canister_ids.json`), so
  dfx finds it by name:

  ```sh
  dfx canister --network ic call settlement books --query
  ```

- `deployments.json` lists every chain's `Swapic`, `Runner` and `CctpReceiver` addresses and
  the block each was deployed at, built from the commit it names.
- Canister upgrades, pool settings and contract deploys run from the private
  `swapic-backend` repository's `staging/` tooling, on `main`. Its README lists the commands.

## Layout

- `contracts/`: the Foundry project: `Swapic.sol` (the door users call), `Runner.sol` (runs
  a swap's steps) and `CctpReceiver.sol` (finishes CCTP arrivals).
- `canister/api`: the canister's candid types, `can.did`, and the conversions between the two.
- `canister/impl`: the canister itself (`lifecycle`, `queries`, `updates`, `guards`,
  `state`, `storage`, `task_manager`).
- `canister/types`: the domain types (checked amounts, identifiers, the quote and event
  codecs) and their golden vectors in `golden/`.
- `canister/integration_tests`: the pocket-ic suite.
- `scripts/`: `build-canister.sh`, `generate-did.sh`, `run-integration-tests.sh`.

## Build and test

```sh
scripts/build-canister.sh settlement   # the wasm, the same bytes CI's wasm job builds
cargo test --locked -p types -p settlement_api -p settlement
scripts/run-integration-tests.sh       # pocket-ic
(cd contracts && forge test -vv)
```

## Changes

Every change reaches `main` through a pull request with green CI (`canister`, `wasm`,
`integration`, `contracts`). Merging does not upgrade the canister: an upgrade is a separate
step run from `swapic-backend`.
