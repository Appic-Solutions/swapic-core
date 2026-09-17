use crate::wasms;
use candid::{encode_one, Principal};
use pocket_ic::PocketIc;
use settlement_api::types::config::Config;
use settlement_api::types::init::InitArg;

pub fn quoter() -> Principal {
    Principal::from_slice(&[2; 29])
}

pub fn watcher() -> Principal {
    Principal::from_slice(&[3; 29])
}

/// A valid install: the spec defaults, and the two test service principals.
pub fn init_arg() -> InitArg {
    InitArg {
        config: Config::default(),
        quoter: quoter(),
        watcher: watcher(),
    }
}

/// (pic, canister, admin); admin is the sole controller, and the canister is installed
/// with `init_arg()`.
pub fn setup() -> (PocketIc, Principal, Principal) {
    let (pic, canister, admin) = empty_canister();
    pic.install_canister(
        canister,
        wasms::settlement(),
        encode_one(init_arg()).unwrap(),
        Some(admin),
    );
    (pic, canister, admin)
}

/// (pic, canister, admin) for a funded canister with no code yet, for a test that installs
/// it itself.
pub fn empty_canister() -> (PocketIc, Principal, Principal) {
    let pic = PocketIc::new();
    let admin = Principal::from_slice(&[1; 29]);
    let canister = pic.create_canister_with_settings(Some(admin), None);
    // every stable structure holds a memory bucket of its own, and suites move the clock by
    // months, so the balance covers that storage and still leaves an upgrade its reserve
    pic.add_cycles(canister, 100_000_000_000_000);
    (pic, canister, admin)
}
