use crate::client::settlement::{events_page, get_config, get_config_full, set_config};
use crate::settlement_suite::init::{setup, upgrade};
use candid::Principal;
use pocket_ic::PocketIc;
use settlement_api::types::config::{Config, ConfigError};
use settlement_api::types::errors::SetConfigError;
use settlement_api::types::events::{Event, EventType};
use std::collections::BTreeMap;

/// Stands in for a real provider url, which is a secret because the key is part of it.
const SECRET_RPC: &str = "https://eth-mainnet.g.alchemy.com/v2/secret-key";

fn events(pic: &PocketIc, canister: Principal, sender: Principal) -> Vec<Event> {
    events_page(pic, canister, sender, 0, 100)
}

/// The public view of `config`, built independently of the canister: every rpc url
/// blanked, every chain id and every other field as written.
fn redacted(config: &Config) -> Config {
    Config {
        rpc_urls: config
            .rpc_urls
            .keys()
            .map(|chain| (*chain, "***".to_string()))
            .collect(),
        ..config.clone()
    }
}

/// What the log records for a config write: JSON of the public view, built here from the
/// wire config the test wrote rather than through the canister's own conversion.
fn logged_json(config: &Config) -> String {
    serde_json::to_string(&redacted(config)).expect("a config is plain JSON")
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

/// The harness installs with the spec defaults, and the install writes them through the
/// path `set_config` takes, so this reads them back through the real path.
#[test]
fn a_fresh_install_seeds_the_defaults() {
    let (pic, canister, admin) = setup();
    assert_eq!(
        get_config_full(&pic, canister, admin).unwrap(),
        Config::default()
    );
}

#[test]
fn admin_set_config_writes_the_value_and_logs_the_change() {
    let (pic, canister, admin) = setup();
    assert_eq!(get_config(&pic, canister, admin), Config::default());

    let before = events(&pic, canister, admin).len();
    let new = with_fee(10);
    set_config(&pic, canister, admin, &new).unwrap();

    assert_eq!(get_config(&pic, canister, admin), new);
    let logged = events(&pic, canister, admin);
    assert_eq!(logged.len(), before + 1, "one config change, one event");
    assert_eq!(
        logged.last().unwrap().payload,
        EventType::ConfigChanged {
            json: logged_json(&new)
        },
        "the event carries the config that was written"
    );
}

#[test]
fn stranger_set_config_is_rejected_and_writes_nothing() {
    let (pic, canister, admin) = setup();
    let stranger = Principal::from_slice(&[9; 29]);
    let before = events(&pic, canister, admin);

    assert!(set_config(&pic, canister, stranger, &with_fee(10)).is_err());

    assert_eq!(
        get_config(&pic, canister, admin),
        Config::default(),
        "a rejected caller changes nothing"
    );
    assert_eq!(events(&pic, canister, admin), before);
}

/// The config is not replayed from the log, so this is the only thing that proves the
/// stable cell is carrying it across. It sets a secret too, because the cell must keep
/// the full config: redaction is a view, not what gets stored.
#[test]
fn config_survives_upgrade() {
    let (pic, canister, admin) = setup();
    let new = with_secret_rpc();
    set_config(&pic, canister, admin, &new).unwrap();

    upgrade(&pic, canister, admin).unwrap();

    assert_eq!(get_config_full(&pic, canister, admin).unwrap(), new);
    assert_eq!(get_config(&pic, canister, admin), redacted(&new));
}

#[test]
fn public_get_config_redacts_rpc_urls() {
    let (pic, canister, admin) = setup();
    let stranger = Principal::from_slice(&[9; 29]);
    let new = with_secret_rpc();
    set_config(&pic, canister, admin, &new).unwrap();

    let public = get_config(&pic, canister, stranger);
    assert_eq!(
        public.rpc_urls,
        BTreeMap::from([(1, "***".to_string())]),
        "the chain id stays visible, the key does not"
    );
    assert_eq!(public, redacted(&new), "and nothing else is hidden");
}

#[test]
fn get_config_full_gives_a_controller_the_real_urls() {
    let (pic, canister, admin) = setup();
    let new = with_secret_rpc();
    set_config(&pic, canister, admin, &new).unwrap();

    assert_eq!(get_config_full(&pic, canister, admin).unwrap(), new);
}

#[test]
fn get_config_full_rejects_a_stranger() {
    let (pic, canister, admin) = setup();
    let stranger = Principal::from_slice(&[9; 29]);
    set_config(&pic, canister, admin, &with_secret_rpc()).unwrap();

    assert!(get_config_full(&pic, canister, stranger).is_err());
}

/// The footgun the redaction created: an operator reads the public view, edits a knob and
/// writes it back, which would store `"***"` as the rpc url and cut the canister off.
#[test]
fn set_config_rejects_a_round_tripped_redacted_config() {
    let (pic, canister, admin) = setup();
    let real = with_secret_rpc();
    set_config(&pic, canister, admin, &real).unwrap();
    let written = events(&pic, canister, admin).len();

    let mut round_tripped = get_config(&pic, canister, admin);
    round_tripped.platform_fee_bps = 20;
    let err = set_config(&pic, canister, admin, &round_tripped).unwrap_err();
    assert_eq!(
        err,
        SetConfigError::InvalidConfig(ConfigError::RedactedRpcUrl { chain_id: 1 }),
        "the error must name the field"
    );

    // the real url is still in place and the rejected write left no event behind
    assert_eq!(get_config_full(&pic, canister, admin).unwrap(), real);
    assert_eq!(events(&pic, canister, admin).len(), written);
}

#[test]
fn set_config_rejects_an_incoherent_fee_and_writes_nothing() {
    let (pic, canister, admin) = setup();
    let before = events(&pic, canister, admin);
    let bad = Config {
        platform_fee_bps: 40,
        max_fee_bps: 30,
        ..Config::default()
    };
    let err = set_config(&pic, canister, admin, &bad).unwrap_err();
    assert_eq!(
        err,
        SetConfigError::InvalidConfig(ConfigError::PlatformFeeAboveMax {
            platform_fee_bps: 40,
            max_fee_bps: 30
        }),
        "the error must name the field"
    );

    assert_eq!(get_config(&pic, canister, admin), Config::default());
    assert_eq!(events(&pic, canister, admin), before);
}

/// `events_page` is a public query, so anything in the log is public.
#[test]
fn config_changed_event_carries_no_rpc_secret() {
    let (pic, canister, admin) = setup();
    let new = with_secret_rpc();
    set_config(&pic, canister, admin, &new).unwrap();

    let logged = events(&pic, canister, admin);
    let EventType::ConfigChanged { json } = &logged.last().unwrap().payload else {
        panic!("a config change logs a ConfigChanged event");
    };
    assert!(
        !json.contains(SECRET_RPC) && !json.contains("secret-key"),
        "the world-readable log leaked an rpc secret: {json}"
    );
    assert!(json.contains("***"), "and it says so: {json}");
    assert_eq!(*json, logged_json(&new));
}
