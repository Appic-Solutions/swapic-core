#!/usr/bin/env bash
# The mainnet-fork run (design 7.1): real Jupiter routes through the real program and real CCTP, on a
# local Surfpool fork. Signatures are not checked there, so no key exists and nothing is ever sent to
# mainnet. Surfpool reads mainnet by the `mainnet` moniker: no URL on any command line.
set -euo pipefail
cd "$(dirname "$0")"
here=$PWD
state="${TMPDIR:-/tmp}/swapic-fork-$$"
mkdir -p "$state" out
export SWAPIC_SOLANA_CONSTS="$here/../tests/consts.test.toml"
(cd .. && cargo-build-sbf --manifest-path program/Cargo.toml 2> /dev/null)
if curl -s -m 2 -X POST -H 'content-type: application/json' -d '{"jsonrpc":"2.0","id":1,"method":"getHealth"}' http://127.0.0.1:8899 > /dev/null; then
  echo "Something already answers on port 8899: this run writes to the node it talks to, so it stops here." >&2
  exit 1
fi
(cd "$state" && exec surfpool start --network mainnet --no-tui --no-studio --no-deploy --skip-signature-verification --port 8899 --ws-port 8900 --log-level warn > "$state/surfpool.out" 2>&1) &
surf=$!
trap 'kill "$surf" 2> /dev/null || true' EXIT
for _ in $(seq 1 60); do
  curl -s -X POST -H 'content-type: application/json' -d '{"jsonrpc":"2.0","id":1,"method":"getHealth"}' http://127.0.0.1:8899 | grep -q ok && break
  sleep 1
done
cd ../tests
cargo run --quiet --bin fork -- "$here/out/last_run.json" "$@"
