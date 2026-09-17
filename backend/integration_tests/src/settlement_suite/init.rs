use crate::wasms;
use candid::Principal;
use pocket_ic::PocketIc;

/// (pic, canister, admin); admin is the sole controller.
pub fn setup() -> (PocketIc, Principal, Principal) {
    let pic = PocketIc::new();
    let admin = Principal::from_slice(&[1; 29]);
    let canister = pic.create_canister_with_settings(Some(admin), None);
    // every stable structure holds a memory bucket of its own, and suites move the clock by
    // months, so the balance covers that storage and still leaves an upgrade its reserve
    pic.add_cycles(canister, 100_000_000_000_000);
    pic.install_canister(
        canister,
        wasms::settlement(),
        candid::encode_args(()).unwrap(),
        Some(admin),
    );
    (pic, canister, admin)
}
