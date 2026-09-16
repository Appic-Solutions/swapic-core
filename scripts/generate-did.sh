#!/usr/bin/env bash
set -euo pipefail

show_help() {
  cat << EOF_HELP
Build the production wasm of a canister and write its candid interface to
backend/canisters/<CANISTER>/api/can.did.
Must be run from the repository's root folder.

Usage:
  scripts/generate-did.sh [options] <CANISTER>

Options:
  -h, --help        Show this message and exit
EOF_HELP
}

while [[ $# -gt 0 && "$1" =~ ^- && "$1" != "--" ]]; do
  case $1 in
    -h | --help )
      show_help
      exit 0
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

# the production build, never the inttest one, so the test doors cannot reach the did
scripts/build-canister.sh "$CANISTER"

DID_PATH="backend/canisters/$CANISTER/api/can.did"
candid-extractor "backend/canisters/$CANISTER/target/wasm32-unknown-unknown/release/$CANISTER.wasm" > "$DID_PATH.tmp"
mv "$DID_PATH.tmp" "$DID_PATH"
echo "Wrote $DID_PATH"
