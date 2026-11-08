#!/usr/bin/env bash
# The whole line on the mainnet fork: the real program, the real CCTP programs and real Jupiter routes, driven by
# the real quoter's builders, read back by the real watcher's reader and passed by the real app guard. Signatures are
# not checked on the fork, so no key exists and nothing is ever sent to mainnet. No URL on any command line: Surfpool
# reads mainnet by its moniker, and the local fork's own address is no secret.
set -euo pipefail
cd "$(dirname "$0")"
here=$PWD
backend=${SWAPIC_BACKEND_DIR:-$here/../../../swapic-backend}
app=${SWAPIC_APP_DIR:-$here/../../../swapic-app}
state="${TMPDIR:-/tmp}/swapic-services-fork-$$"
mkdir -p "$state" out
export SWAPIC_SOLANA_CONSTS="$here/../tests/consts.test.toml"
(cd .. && cargo-build-sbf --manifest-path program/Cargo.toml 2> /dev/null)
if curl -s -m 2 -X POST -H 'content-type: application/json' -d '{"jsonrpc":"2.0","id":1,"method":"getHealth"}' http://127.0.0.1:8899 > /dev/null; then
  echo "Something already answers on port 8899: this run writes to the node it talks to, so it stops here." >&2
  exit 1
fi
(cd "$state" && exec surfpool start --network mainnet --no-tui --no-studio --no-deploy --skip-signature-verification --port 8899 > "$state/surfpool.log" 2>&1) &
surf=$!
hold="$here/out/hold.json"
rm -f "$hold" "$hold.stop"
trap 'touch "$hold.stop"; kill "$surf" 2> /dev/null || true' EXIT
for _ in $(seq 1 60); do
  curl -s -X POST -H 'content-type: application/json' -d '{"jsonrpc":"2.0","id":1,"method":"getHealth"}' http://127.0.0.1:8899 > /dev/null && break
  sleep 1
done
(cd ../tests && cargo run --quiet --bin fork -- "$hold" hold) &
for _ in $(seq 1 180); do
  [[ -s "$hold" ]] && break
  sleep 1
done
[[ -s "$hold" ]] || { echo "the fork was not set up"; exit 1; }
export SOLANA_FORK_HOLD="$hold"
export SOLANA_FORK_ANSWERS="$here/out/answers.json"
echo "== the quoter's builders on the fork"
(cd "$backend" && cargo test -p quoter --lib engine::build::solana::tests::fork -- --ignored --nocapture --test-threads=1)
echo "== the watcher reads what landed"
(cd "$backend" && cargo test -p watcher --bin watcher execution::solana::tests::fork -- --ignored --nocapture --test-threads=1)
echo "== the app's guard passes what the quoter built"
(cd "$app" && pnpm exec vitest run src/features/swap/guards/solana.fork.test.ts)
