## [unreleased]

### 🚀 Features

- Vault storage, roles, uups upgrade auth
- Pause machinery split by class
- Deposit entry with duplicate-hash revert and actual-amount accounting
- Execute with allowlist, exact approvals, balance-delta minimums
- Cross-swap executeMany with isolated items and canister multicall
- Gasless 2612 pull, front-run safe
- Permit2 pull with quote-hash witness
- Atomic depositAndExecute for legacy same-chain swaps
- Payout and refund doors
- Deterministic vault deploy script

### 🐛 Bug Fixes

- Pin placeholder test pragma
- Scope quote-hash marking per payer to stop burn griefing
- No-copy calls, self-target rejection, zero-canister guard, safecast
- Permit2 pull credits measured balance delta
- Public depositAndExecute can only spend the caller's own deposit
- Atomic path gets its own event plus ruled regression tests
- Atomic payout leg goes through the send door

### 🧪 Testing

- Assert deposited event carries received amount for fee tokens
- Access-control, pause-class, reentrancy and return-bomb regressions
- Fuzz and invariant suite for vault
- Standing vacuity guards for the invariant campaign

### ⚙️ Miscellaneous Tasks

- Scaffold swapic-core with foundry, ci, changelog, spec
- Keep docs local, untracked
- Pin foundry toolchain and evm version
