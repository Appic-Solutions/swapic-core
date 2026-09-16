#!/usr/bin/env bash
set -euo pipefail

show_help() {
  cat << EOF_HELP
Build the integration-test wasm and run the integration tests.
Must be run from the repository's root folder.

Usage:
  scripts/run-integration-tests.sh [options] [-- <cargo test args>]

Options:
  -n, --no-build    Only run the tests, without rebuilding the canister wasm
  -h, --help        Show this message and exit

Environment:
  POCKET_IC_BIN     The pocket-ic server to run against. Defaults to the one in the
                    dfx cache, whose major version must match the pocket-ic crate.
EOF_HELP
}

BUILD=1

while [[ $# -gt 0 && "$1" =~ ^- && "$1" != "--" ]]; do
  case $1 in
    -h | --help )
      show_help
      exit 0
      ;;
    -n | --no-build )
      BUILD=0
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

if [[ $BUILD == 1 ]]; then
  scripts/build-canister.sh --integration-test settlement
fi

if [[ -z "${POCKET_IC_BIN:-}" ]]; then
  POCKET_IC_BIN="$(dfx cache show)/pocket-ic"
fi
export POCKET_IC_BIN

cargo test --locked -p integration_tests "$@"
