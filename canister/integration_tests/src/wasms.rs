pub fn settlement() -> Vec<u8> {
    // the integration build has its own target tree so the production wasm path never
    // holds an inttest artifact
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../target/inttest/wasm32-unknown-unknown/release/settlement.wasm"
    );
    std::fs::read(path).expect("run `scripts/run-integration-tests.sh` first")
}

/// The ICRC-1/ICRC-2 ledger wasm the pool's suite runs ckUSDC on: `LEDGER_WASM`, or the
/// legacy evm-minter's `ledger_canister_u256.wasm.gz` beside the repository, which is not
/// committed here.
pub fn icrc1_ledger() -> Vec<u8> {
    let candidates: Vec<std::path::PathBuf> = match std::env::var("LEDGER_WASM") {
        Ok(path) => vec![path.into()],
        Err(_) => ["../../../legacy", "../../../../legacy"]
            .iter()
            .map(|up| {
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join(up)
                    .join("evm-minter/wasm/ledger_canister_u256.wasm.gz")
            })
            .collect(),
    };
    candidates
        .iter()
        .find_map(|path| std::fs::read(path).ok())
        .expect("set LEDGER_WASM to an ICRC-1 ledger wasm (ledger_canister_u256.wasm.gz)")
}
