// every test binary compiles the whole module and uses only the part it needs
#![allow(dead_code)]

use candid::{decode_one, encode_one, CandidType, Deserialize, Principal};
use pocket_ic::PocketIc;
use settlement::events::Event;

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

/// The test door onto `append_event`; the inner Result is the canister's own answer.
pub fn append(
    pic: &PocketIc,
    canister: Principal,
    sender: Principal,
    event: &Event,
) -> Result<u64, String> {
    let raw = pic
        .update_call(canister, sender, "test_append", encode_one(event).unwrap())
        .unwrap();
    decode_one(&raw).unwrap()
}

/// `args` is pre-encoded so callers can pass any arity.
pub fn query<T: CandidType + for<'de> Deserialize<'de>>(
    pic: &PocketIc,
    canister: Principal,
    sender: Principal,
    method: &str,
    args: Vec<u8>,
) -> T {
    let raw = pic.query_call(canister, sender, method, args).unwrap();
    decode_one(&raw).unwrap()
}
