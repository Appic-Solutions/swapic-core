POCKET_IC_BIN ?= $(shell dfx cache show)/pocket-ic
export POCKET_IC_BIN

wasm:
	cargo build --locked --target wasm32-unknown-unknown --release -p settlement

wasm-test:
	cargo build --locked --target-dir target/test --target wasm32-unknown-unknown --release -p settlement --features test-endpoints

test: wasm-test
	cargo test --locked -p settlement

did: wasm
	candid-extractor target/wasm32-unknown-unknown/release/settlement.wasm > canister/settlement/settlement.did
