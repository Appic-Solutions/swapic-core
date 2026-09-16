#!/usr/bin/env bash
set -euo pipefail

show_help() {
  cat << EOF_HELP
Build a canister wasm.
Must be run from the repository's root folder.

Usage:
  scripts/build-canister.sh [options] <CANISTER>

Options:
  -h, --help                Show this message and exit
  -it, --integration-test   Build with the inttest feature, into its own target directory

Outputs:
  production        backend/canisters/<CANISTER>/target/wasm32-unknown-unknown/release/<CANISTER>.wasm
  integration test  backend/canisters/<CANISTER>/target/inttest/wasm32-unknown-unknown/release/<CANISTER>.wasm
EOF_HELP
}

BASE_CANISTER_PATH="backend/canisters"
INTTEST=0

while [[ $# -gt 0 && "$1" =~ ^- && "$1" != "--" ]]; do
  case $1 in
    -h | --help )
      show_help
      exit 0
      ;;
    -it | --integration-test )
      INTTEST=1
      ;;
    * )
      echo "Error: unknown option $1" >&2
      show_help >&2
      exit 1
      ;;
  esac
  shift
done
if [[ $# -gt 0 && "$1" == "--" ]]; then shift; fi

if [[ $# -ne 1 ]]; then
  echo "Error: missing <CANISTER> argument" >&2
  show_help >&2
  exit 1
fi
CANISTER=$1
TARGET_DIR="$BASE_CANISTER_PATH/$CANISTER/target"

if [[ $INTTEST == 1 ]]; then
  echo "Building canister $CANISTER for integration testing"
  # its own target directory, so the production wasm path can never hold an
  # integration-test artifact
  cargo build --locked --target wasm32-unknown-unknown --release \
    --target-dir "$TARGET_DIR/inttest" -p "$CANISTER" --features inttest
else
  echo "Building canister $CANISTER"
  cargo build --locked --target wasm32-unknown-unknown --release \
    --target-dir "$TARGET_DIR" -p "$CANISTER"
fi
