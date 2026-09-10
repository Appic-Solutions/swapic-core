POCKET_IC_BIN ?= $(shell dfx cache show)/pocket-ic
export POCKET_IC_BIN

wasm:
	cargo build --target wasm32-unknown-unknown --release -p settlement

wasm-test:
	cargo build --target wasm32-unknown-unknown --release -p settlement --features test-endpoints

test: wasm-test
	cargo test -p settlement

did: wasm
	candid-extractor target/wasm32-unknown-unknown/release/settlement.wasm > canister/settlement/settlement.did
