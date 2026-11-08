#!/usr/bin/env bash
# Builds the program with the test constants and the stand-in router, then runs the program's tests.
# The short-rent build proves that a user-paid burn cannot draw on the float (attack L6).
set -euo pipefail
cd "$(dirname "$0")"
here=$PWD
cargo run --quiet --bin fetch_fixtures
cd ..
export SWAPIC_SOLANA_CONSTS="$here/consts.test.toml"
cargo-build-sbf --manifest-path test_router/Cargo.toml 2> /dev/null
cargo-build-sbf --manifest-path program/Cargo.toml --features test-short-rent 2> /dev/null
cp target/deploy/swapic_solana_program.so target/deploy/swapic_solana_program_short_rent.so
cargo-build-sbf --manifest-path program/Cargo.toml 2> /dev/null
cd "$here"
cargo test "$@"
