#!/usr/bin/env bash
set -euo pipefail

show_help() {
  cat << EOF_HELP
Write a canister's candid interface, as its own test build exports it, to
canister/api/can.did.
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

# the production wasm exports no candid: the interface is read off the canister's own
# test build (the same macro output, never the inttest doors) by the test that pins it
UPDATE_DID=1 cargo test --locked -p "$CANISTER" --lib -- the_committed_did_is_the_exported_one
echo "Wrote canister/api/can.did"
