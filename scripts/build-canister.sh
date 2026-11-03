#!/usr/bin/env bash
set -euo pipefail

show_help() {
  cat << EOF_HELP
Build a canister wasm.
Must be run from the repository's root folder.

The build is reproducible across machines: the compiler is the release rust-toolchain.toml
pins (a RUSTUP_TOOLCHAIN left in the shell is ignored), and every path that names this
machine (the source root, the cargo home, the toolchain's sysroot) is remapped to a fixed
prefix, so the same commit gives the same wasm wherever it is built.

Usage:
  scripts/build-canister.sh [options] <CANISTER>

Options:
  -h, --help                Show this message and exit
  -it, --integration-test   Build with the inttest feature, into its own target directory
  --toolchain <NAME>        Build with the rustup toolchain NAME in place of
                            rust-toolchain.toml's; refused unless its rustc is the pinned
                            release (for checking that another install builds the same wasm)

Outputs:
  production        canister/target/wasm32-unknown-unknown/release/<CANISTER>.wasm
  integration test  canister/target/inttest/wasm32-unknown-unknown/release/<CANISTER>.wasm
EOF_HELP
}

BASE_CANISTER_PATH="canister"
INTTEST=0
TOOLCHAIN=""

while [[ $# -gt 0 && "$1" =~ ^- && "$1" != "--" ]]; do
  case $1 in
    -h | --help )
      show_help
      exit 0
      ;;
    -it | --integration-test )
      INTTEST=1
      ;;
    --toolchain )
      if [[ $# -lt 2 ]]; then
        echo "Error: --toolchain needs a toolchain name" >&2
        exit 1
      fi
      TOOLCHAIN=$2
      shift
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
TARGET_DIR="$BASE_CANISTER_PATH/target"

# The compiler: rust-toolchain.toml's release, never one a shell variable picks. A toolchain
# asked by name must be that same release.
PINNED=$(sed -n -E 's/^channel *= *"([^"]+)".*/\1/p' rust-toolchain.toml)
if [[ -z "$PINNED" ]]; then
  echo "Error: rust-toolchain.toml names no channel" >&2
  exit 1
fi
if [[ -n "${RUSTUP_TOOLCHAIN:-}" ]]; then
  echo "Ignoring RUSTUP_TOOLCHAIN=$RUSTUP_TOOLCHAIN: rust-toolchain.toml pins $PINNED"
  unset RUSTUP_TOOLCHAIN
fi
if [[ -n "$TOOLCHAIN" ]]; then
  export RUSTUP_TOOLCHAIN=$TOOLCHAIN
fi
RUSTC_RELEASE=$(rustc -vV | sed -n 's/^release: //p')
RUSTC_COMMIT=$(rustc -vV | sed -n 's/^commit-hash: //p')
if [[ "$RUSTC_RELEASE" != "$PINNED" ]]; then
  echo "Error: rustc is $RUSTC_RELEASE, not $PINNED as rust-toolchain.toml pins it" >&2
  exit 1
fi
echo "rustc $RUSTC_RELEASE ($RUSTC_COMMIT) from toolchain ${TOOLCHAIN:-$PINNED}"

# Flags from the shell would change the wasm: only this script's are used.
if [[ -n "${RUSTFLAGS:-}" || -n "${CARGO_ENCODED_RUSTFLAGS:-}" ]]; then
  echo "Error: RUSTFLAGS or CARGO_ENCODED_RUSTFLAGS is set; the wasm is built with this script's flags only" >&2
  exit 1
fi

# Every machine path the compiler could write into the wasm, remapped to a fixed prefix. When
# several match, rustc applies the last, so the most specific comes last. A path is remapped
# as given and as the file system resolves it, so a symlink cannot keep the machine's path.
REMAPS=()
remap() {
  local from=$1 to=$2 real
  REMAPS+=("--remap-path-prefix=$from=$to")
  if real=$(cd "$from" 2> /dev/null && pwd -P) && [[ "$real" != "$from" ]]; then
    REMAPS+=("--remap-path-prefix=$real=$to")
  fi
}
CARGO_HOME_DIR=${CARGO_HOME:-$HOME/.cargo}
SYSROOT=$(rustc --print sysroot)
# the source root, which holds the target directory and its build scripts' output
remap "$PWD" /swapic
# the registry's and git's sources
remap "$CARGO_HOME_DIR" /cargo
remap "$CARGO_HOME_DIR/registry" /cargo/registry
remap "$CARGO_HOME_DIR/git" /cargo/git
remap "$SYSROOT" /rust
# with the rust-src component, rustc reads std's sources from the sysroot and writes their
# local paths; without it, the virtual /rustc/<commit> paths std carries: remapped to those
remap "$SYSROOT/lib/rustlib/src/rust" "/rustc/$RUSTC_COMMIT"
CARGO_ENCODED_RUSTFLAGS=$(
  IFS=$'\x1f'
  echo "${REMAPS[*]}"
)
export CARGO_ENCODED_RUSTFLAGS

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
  # the production wasm with what no call reaches removed: a smaller module, the same
  # behaviour, and as reproducible as the build itself
  # pinned like the compiler: another ic-wasm may shrink to other bytes
  IC_WASM_VERSION="0.9.10"
  if ! command -v ic-wasm > /dev/null; then
    echo "Error: ic-wasm $IC_WASM_VERSION is needed to shrink the wasm (cargo install ic-wasm --version $IC_WASM_VERSION --locked)" >&2
    exit 1
  fi
  if [[ "$(ic-wasm --version)" != "ic-wasm $IC_WASM_VERSION" ]]; then
    echo "Error: $(ic-wasm --version) found, the build pins ic-wasm $IC_WASM_VERSION" >&2
    exit 1
  fi
  WASM="$TARGET_DIR/wasm32-unknown-unknown/release/$CANISTER.wasm"
  ic-wasm "$WASM" -o "$WASM" shrink
fi
