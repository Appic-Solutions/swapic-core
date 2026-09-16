pub fn settlement() -> Vec<u8> {
    // the integration build has its own target tree so the production wasm path never
    // holds an inttest artifact
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../canisters/settlement/target/inttest/wasm32-unknown-unknown/release/settlement.wasm"
    );
    std::fs::read(path).expect("run `scripts/run-integration-tests.sh` first")
}
