use crate::wasms;
use candid::{encode_one, Principal};
use pocket_ic::{ErrorCode, PocketIc, RejectResponse};
use settlement_api::types::config::Config;
use settlement_api::types::init::InitArg;
use std::collections::BTreeMap;
use std::time::Duration;

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
    install(&pic, canister, admin, &init_arg()).expect("the default arg installs");
    (pic, canister, admin)
}

/// The rails' USDC on the two chains the suite's quotes name, Base's and Arbitrum's: what a
/// registration pins a quote's tokens to (rule A5), as the claim does. The one helper every
/// suite that registers a quote names them with.
pub fn rail_usdc() -> BTreeMap<u64, String> {
    BTreeMap::from([
        (
            8453,
            "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".to_string(),
        ),
        (
            42161,
            "0xaf88d065e77c8cC2239327C5EDb3A432268e5831".to_string(),
        ),
    ])
}

/// The spec defaults with the rails' USDC named: the config a suite registers quotes under.
pub fn rail_config() -> Config {
    Config {
        usdc_addresses: rail_usdc(),
        ..Config::default()
    }
}

/// `setup()` installed with [`rail_config`], for a suite that registers quotes.
pub fn setup_with_rail_usdc() -> (PocketIc, Principal, Principal) {
    let (pic, canister, admin) = empty_canister();
    let arg = InitArg {
        config: rail_config(),
        ..init_arg()
    };
    install(&pic, canister, admin, &arg).expect("the default arg with the rails' USDC installs");
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

/// Installs the wasm the suite builds, and answers what the management canister answered.
/// Goes through the fallible reinstall, which on a canister with no code is an install, so
/// a refused install is a value here and not a panic inside the harness.
pub fn install(
    pic: &PocketIc,
    canister: Principal,
    admin: Principal,
    arg: &InitArg,
) -> Result<(), RejectResponse> {
    let arg = encode_one(arg).expect("an init arg is candid");
    through_the_install_rate_limit(pic, || {
        pic.reinstall_canister(canister, wasms::settlement(), arg.clone(), Some(admin))
    })
}

/// Upgrades the canister onto the wasm the suite builds, and answers what the management
/// canister answered.
pub fn upgrade(
    pic: &PocketIc,
    canister: Principal,
    admin: Principal,
) -> Result<(), RejectResponse> {
    through_the_install_rate_limit(pic, || {
        pic.upgrade_canister(
            canister,
            wasms::settlement(),
            encode_one(()).expect("the unit arg is candid"),
            Some(admin),
        )
    })
}

/// A subnet that has just compiled a wasm this size refuses the next `install_code` with
/// `CanisterInstallCodeRateLimited` until it has worked that instruction debt off, a round
/// at a time. That refusal is about the size of this module and never about the canister
/// under test, so every install and upgrade in the suite ticks through it here: a suite
/// that installs, upgrades and reinstalls within one instant is not what any operator does,
/// and a test asserting a refusal the canister itself made still reads it from the answer.
fn through_the_install_rate_limit(
    pic: &PocketIc,
    mut install_code: impl FnMut() -> Result<(), RejectResponse>,
) -> Result<(), RejectResponse> {
    for _ in 0..30 {
        match install_code() {
            Err(rejected) if rejected.error_code == ErrorCode::CanisterInstallCodeRateLimited => {
                pic.advance_time(Duration::from_secs(60));
                pic.tick();
            }
            answer => return answer,
        }
    }
    panic!("the install-code budget never recovered");
}
