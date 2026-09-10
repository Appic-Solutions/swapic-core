mod common;

use candid::{decode_one, encode_args, encode_one, Principal};
use pocket_ic::PocketIc;
use settlement::config::Config;
use settlement::events::{Event, EventEnvelope};

fn get_config(pic: &PocketIc, canister: Principal, sender: Principal) -> Config {
    common::query(pic, canister, sender, "get_config", encode_one(()).unwrap())
}

fn set_config(
    pic: &PocketIc,
    canister: Principal,
    sender: Principal,
    new: &Config,
) -> Result<(), String> {
    let raw = pic
        .update_call(canister, sender, "set_config", encode_one(new).unwrap())
        .unwrap();
    decode_one(&raw).unwrap()
}

fn events(pic: &PocketIc, canister: Principal, sender: Principal) -> Vec<EventEnvelope> {
    common::query(
        pic,
        canister,
        sender,
        "events_page",
        encode_args((0u64, 100u64)).unwrap(),
    )
}

/// The knob the launch config actually moves.
fn with_fee(bps: u16) -> Config {
    Config {
        platform_fee_bps: bps,
        ..Config::default()
    }
}

#[test]
fn admin_set_config_writes_the_value_and_logs_the_change() {
    let (pic, canister, admin) = common::setup();
    assert_eq!(get_config(&pic, canister, admin), Config::default());

    let new = with_fee(10);
    set_config(&pic, canister, admin, &new).unwrap();

    assert_eq!(get_config(&pic, canister, admin), new);
    let logged = events(&pic, canister, admin);
    assert_eq!(logged.len(), 1, "one config change, one event");
    assert_eq!(
        logged[0].event,
        Event::ConfigChanged {
            json: format!("{new:?}")
        },
        "the event carries the config that was written"
    );
}

#[test]
fn stranger_set_config_is_rejected_and_writes_nothing() {
    let (pic, canister, admin) = common::setup();
    let stranger = Principal::from_slice(&[9; 29]);

    assert!(set_config(&pic, canister, stranger, &with_fee(10)).is_err());

    assert_eq!(
        get_config(&pic, canister, admin),
        Config::default(),
        "a rejected caller changes nothing"
    );
    assert!(events(&pic, canister, admin).is_empty());
}

/// The config is not replayed from the log, so this is the only thing that proves the
/// stable cell is carrying it across.
#[test]
fn config_survives_upgrade() {
    let (pic, canister, admin) = common::setup();
    let new = with_fee(10);
    set_config(&pic, canister, admin, &new).unwrap();

    pic.upgrade_canister(
        canister,
        common::wasm(),
        encode_one(()).unwrap(),
        Some(admin),
    )
    .unwrap();

    assert_eq!(get_config(&pic, canister, admin), new);
}
