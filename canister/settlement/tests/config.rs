mod common;

use candid::{decode_one, encode_args, encode_one, Principal};
use pocket_ic::PocketIc;
use settlement::config::Config;
use settlement::events::{Event, EventEnvelope};
use std::collections::BTreeMap;

/// Stands in for a real provider url, which is a secret because the key is part of it.
const SECRET_RPC: &str = "https://eth-mainnet.g.alchemy.com/v2/secret-key";

fn get_config(pic: &PocketIc, canister: Principal, sender: Principal) -> Config {
    common::query(pic, canister, sender, "get_config", encode_one(()).unwrap())
}

fn get_config_full(
    pic: &PocketIc,
    canister: Principal,
    sender: Principal,
) -> Result<Config, String> {
    common::query(
        pic,
        canister,
        sender,
        "get_config_full",
        encode_one(()).unwrap(),
    )
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

/// A config carrying a secret, which is what a real deploy looks like.
fn with_secret_rpc() -> Config {
    Config {
        rpc_urls: BTreeMap::from([(1, SECRET_RPC.to_string())]),
        ..with_fee(10)
    }
}

/// `init` seeds the cell with `Config::default()` and nothing validates on the way in, so
/// this reads the seed back through the real path. Paired with the `defaults_are_valid`
/// unit test, it pins that what a fresh install stores is a config `set` would accept.
#[test]
fn a_fresh_install_seeds_the_defaults() {
    let (pic, canister, admin) = common::setup();
    assert_eq!(
        get_config_full(&pic, canister, admin).unwrap(),
        Config::default()
    );
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
            json: format!("{:?}", new.redacted())
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
/// stable cell is carrying it across. It sets a secret too, because the cell must keep
/// the full config: redaction is a view, not what gets stored.
#[test]
fn config_survives_upgrade() {
    let (pic, canister, admin) = common::setup();
    let new = with_secret_rpc();
    set_config(&pic, canister, admin, &new).unwrap();

    pic.upgrade_canister(
        canister,
        common::wasm(),
        encode_one(()).unwrap(),
        Some(admin),
    )
    .unwrap();

    assert_eq!(get_config_full(&pic, canister, admin).unwrap(), new);
    assert_eq!(get_config(&pic, canister, admin), new.redacted());
}

#[test]
fn public_get_config_redacts_rpc_urls() {
    let (pic, canister, admin) = common::setup();
    let stranger = Principal::from_slice(&[9; 29]);
    let new = with_secret_rpc();
    set_config(&pic, canister, admin, &new).unwrap();

    let public = get_config(&pic, canister, stranger);
    assert_eq!(
        public.rpc_urls,
        BTreeMap::from([(1, "***".to_string())]),
        "the chain id stays visible, the key does not"
    );
    assert_eq!(public, new.redacted(), "and nothing else is hidden");
}

#[test]
fn get_config_full_gives_a_controller_the_real_urls() {
    let (pic, canister, admin) = common::setup();
    let new = with_secret_rpc();
    set_config(&pic, canister, admin, &new).unwrap();

    assert_eq!(get_config_full(&pic, canister, admin).unwrap(), new);
}

#[test]
fn get_config_full_rejects_a_stranger() {
    let (pic, canister, admin) = common::setup();
    let stranger = Principal::from_slice(&[9; 29]);
    set_config(&pic, canister, admin, &with_secret_rpc()).unwrap();

    assert!(get_config_full(&pic, canister, stranger).is_err());
}

/// The footgun the redaction created: an operator reads the public view, edits a knob and
/// writes it back, which would store `"***"` as the rpc url and cut the canister off.
#[test]
fn set_config_rejects_a_round_tripped_redacted_config() {
    let (pic, canister, admin) = common::setup();
    let real = with_secret_rpc();
    set_config(&pic, canister, admin, &real).unwrap();

    let mut round_tripped = get_config(&pic, canister, admin);
    round_tripped.platform_fee_bps = 20;
    let err = set_config(&pic, canister, admin, &round_tripped).unwrap_err();
    assert!(
        err.contains("rpc_urls"),
        "the error must name the field: {err}"
    );

    // the real url is still in place and the rejected write left no event behind
    assert_eq!(get_config_full(&pic, canister, admin).unwrap(), real);
    assert_eq!(events(&pic, canister, admin).len(), 1);
}

#[test]
fn set_config_rejects_an_incoherent_fee_and_writes_nothing() {
    let (pic, canister, admin) = common::setup();
    let bad = Config {
        platform_fee_bps: 40,
        max_fee_bps: 30,
        ..Config::default()
    };
    let err = set_config(&pic, canister, admin, &bad).unwrap_err();
    assert!(
        err.contains("platform_fee_bps"),
        "the error must name the field: {err}"
    );

    assert_eq!(get_config(&pic, canister, admin), Config::default());
    assert!(events(&pic, canister, admin).is_empty());
}

/// `events_page` is a public query, so anything in the log is public.
#[test]
fn config_changed_event_carries_no_rpc_secret() {
    let (pic, canister, admin) = common::setup();
    let new = with_secret_rpc();
    set_config(&pic, canister, admin, &new).unwrap();

    let logged = events(&pic, canister, admin);
    let Event::ConfigChanged { json } = &logged[0].event else {
        panic!("a config change logs a ConfigChanged event");
    };
    assert!(
        !json.contains(SECRET_RPC) && !json.contains("secret-key"),
        "the world-readable log leaked an rpc secret: {json}"
    );
    assert!(json.contains("***"), "and it says so: {json}");
    assert_eq!(*json, format!("{:?}", new.redacted()));
}
