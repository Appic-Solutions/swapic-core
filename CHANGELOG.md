## [unreleased]

### 🚀 Features

- Hash-chained event envelope
- Swap state machine with pure apply and sign-before-send guard
- Stable event log with upgrade-safe replay
- Config store with spec defaults
- Roles, canonical quote hash, pending gasless store
- Expiry and replay-audit timers with halt switch

### 🐛 Bug Fixes

- Public atomic path emits only its own event
- Executions pause gates the atomic path, no native sends to zero
- Separate test wasm output, pin dfx action
- Hand-written canonical event encoding for the hash chain
- Tighten transition guard, add pocket release event
- Waiting clock hygiene and pocket settle-debit
- Keep rpc api keys out of the public config and the event log
- Config hardening, exhaustive redaction and set validation
- Quote layout in the interface, roles audit event, pending caps

### 📚 Documentation

- Name the canister-only events the atomic path must not forge

### 🧪 Testing

- Permit2 access gate, fresh contracts readme
- Harden the event codec golden guard
- Pin storage round-trip and deepen the spine coverage

### ⚙️ Miscellaneous Tasks

- Run fork tests with the base rpc secret
- Settlement canister crate, rust ci, makefile
## [contracts-v0.1.0] - 2026-09-06

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
- Changelog
