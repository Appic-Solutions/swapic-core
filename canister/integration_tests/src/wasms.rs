pub fn settlement() -> Vec<u8> {
    // the integration build has its own target tree so the production wasm path never
    // holds an inttest artifact
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../target/inttest/wasm32-unknown-unknown/release/settlement.wasm"
    );
    std::fs::read(path).expect("run `scripts/run-integration-tests.sh` first")
}

/// The stub of the ck minter the pool's tests run on: `canister/stub_minter`, never deployed.
pub fn stub_minter() -> Vec<u8> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../target/inttest/wasm32-unknown-unknown/release/stub_minter.wasm"
    );
    std::fs::read(path).expect("run `scripts/run-integration-tests.sh` first")
}

/// The integration build of the commit deployed now, for the upgrade from it: `PRIOR_WASM`, a
/// wasm built with `scripts/build-canister.sh --integration-test settlement` at that commit.
pub fn prior_settlement() -> Vec<u8> {
    let path = std::env::var("PRIOR_WASM")
        .expect("set PRIOR_WASM to the deployed commit's integration build");
    std::fs::read(path).expect("PRIOR_WASM names no file")
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
