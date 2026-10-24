# Swapic (contracts)

Foundry project for three contracts per EVM chain (design C). None of them knows a rail, a DEX or a fee: the quoter writes a plan of calls, the user signs it, and the contracts run it under rules they check themselves.

- `Swapic` (the door, the only contract users approve): `run`, `runWithPermit2`, `runFor` (the canister only: EIP-3009, EIP-2612 plus a signed QuoteAcceptance, Permit2 witness). It pulls exactly the signed `amountIn` from the payer, once per order id (EIP-712, domain `Swapic`/`3`), into the Runner. Its constructor deploys the CctpReceiver, then the Runner. Owner settings: pause (also the guardian), unpause, `setCanister`, `setGuardian`, two-step ownership.
- `Runner` (the hands): `execute` runs the signed steps, writing the real amounts in at byte offsets, and ends with every tracked token back at its starting balance or reverts. `fill` takes an arrival from anyone (Eco's executor, the CCTP receiver) and either swaps all of it into at least `minOut` of `tokenOut` for the recipient, or deposits all of it with the ck helper, or refunds all of it. It reverts instead (nothing arrives) when paused, while a Swapic door runs, on a bad envelope or recipient, or when the action gets less gas than the envelope's signed `minGas` or is starved (`OutOfGas`: retry with more gas). It refuses calls to our contracts, Permit2, addresses without code, and the token-grant selectors.
- `CctpReceiver` (the only CCTP code): `completeCctp` (the canister or the recipient, before the envelope deadline) and `release` (anyone, after it, even paused).

Commands:

- Build: `forge build` (`via_ir` on, target shanghai).
- Unit tests: `forge test` (the fork tests skip without RPC variables).
- Fork tests: `FOUNDRY_PROFILE=fork fork-env.sh forge test --match-path 'test/fork/*'` (the fork profile targets Cancun: KyberSwap's executor on Base needs its opcodes; the deployed build stays Shanghai). Fixtures in `test/fixtures/` record the block they were fetched at; each test forks at that block.
- Deploy: `script/DeploySwapic.s.sol`, env `CANISTER` (required), `OWNER`, `GUARDIAN`, `CK_HELPER` (Ethereum only). One transaction per chain deploys all three.
