use candid::Principal;
use pocket_ic::PocketIc;

pub fn wasm() -> Vec<u8> {
    // the test build has its own target tree so the production wasm path never holds a
    // test-endpoints artifact
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/test/wasm32-unknown-unknown/release/settlement.wasm"
    );
    std::fs::read(path).expect("run `make wasm-test` first")
}

/// (pic, canister, admin); admin is the sole controller.
pub fn setup() -> (PocketIc, Principal, Principal) {
    let pic = PocketIc::new();
    let admin = Principal::from_slice(&[1; 29]);
    let canister = pic.create_canister_with_settings(Some(admin), None);
    pic.add_cycles(canister, 2_000_000_000_000);
    pic.install_canister(
        canister,
        wasm(),
        candid::encode_args(()).unwrap(),
        Some(admin),
    );
    (pic, canister, admin)
}
