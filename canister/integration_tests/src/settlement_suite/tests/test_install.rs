use crate::client::settlement::{event_count, events_page, get_config_full, test_put_reading};
use crate::settlement_suite::init::{empty_canister, init_arg, install, quoter, watcher};
use candid::{encode_one, Principal};
use pocket_ic::PocketIc;
use settlement_api::types::chain_data::ChainData;
use settlement_api::types::config::Config;
use settlement_api::types::events::EventType;
use settlement_api::types::init::InitArg;
use std::collections::BTreeMap;

/// An install the canister refuses.
fn refused_install(pic: &PocketIc, canister: Principal, admin: Principal, arg: &InitArg) -> String {
    install(pic, canister, admin, arg)
        .expect_err("the install is refused")
        .reject_message
}

/// The install is the first config write and the first role rotation, so it refuses what
/// `set_config` and `set_roles` refuse, and a refused install leaves no code behind: the
/// canister answers nothing. A canister refused once then takes a valid arg, so what
/// refused was the arg.
#[test]
fn an_install_with_an_invalid_arg_is_refused() {
    let (pic, _, admin) = empty_canister();
    // a canister of its own per install: install_code is rate limited per canister
    let fresh = || {
        let canister = pic.create_canister_with_settings(Some(admin), None);
        pic.add_cycles(canister, 100_000_000_000_000);
        canister
    };
    let slow_backstop = InitArg {
        config: Config {
            backstop_interval_s: 3_600,
            ..Config::default()
        },
        ..init_arg()
    };
    let redacted_url = InitArg {
        config: Config {
            rpc_urls: BTreeMap::from([(1, "***".to_string())]),
            ..Config::default()
        },
        ..init_arg()
    };
    let anonymous_quoter = InitArg {
        quoter: Principal::anonymous(),
        ..init_arg()
    };
    for (arg, why) in [
        (slow_backstop, "backstop_interval_s"),
        (redacted_url, "rpc_urls[1]"),
        (anonymous_quoter, "anonymous"),
    ] {
        let canister = fresh();
        let message = refused_install(&pic, canister, admin, &arg);
        assert!(message.contains("install refused"), "{message}");
        assert!(message.contains(why), "name what is wrong: {message}");
        assert!(
            pic.query_call(canister, admin, "event_count", encode_one(()).unwrap())
                .is_err(),
            "a refused install leaves no code to answer"
        );
    }

    let canister = fresh();
    let anonymous_watcher = InitArg {
        watcher: Principal::anonymous(),
        ..init_arg()
    };
    refused_install(&pic, canister, admin, &anonymous_watcher);
    // the refused install's instructions still count against the next one; `install` ticks
    // through that rate limit itself, so there is nothing to wait out by hand here
    install(&pic, canister, admin, &init_arg()).expect("a valid arg installs");
    assert_eq!(event_count(&pic, canister, admin), 2);
}

/// A fresh install is in service at once: the arg's config is the stored one, its quoter
/// and watcher hold the roles, and the log opens with the two audit lines that record them.
#[test]
fn a_fresh_install_has_the_args_config_roles_and_audit_events() {
    let (pic, canister, admin) = empty_canister();
    // the rails' USDC is named, which the store pins a registered quote's tokens to
    let config = Config {
        rpc_urls: BTreeMap::from([(1, "https://eth.example/v2/secret-key".to_string())]),
        ecdsa_key_name: "key_1".to_string(),
        ..Config::default()
    };
    let arg = InitArg {
        config: config.clone(),
        ..init_arg()
    };
    install(&pic, canister, admin, &arg).expect("a valid arg installs");

    assert_eq!(get_config_full(&pic, canister, admin).unwrap(), config);

    // the arg's watcher holds its role: its door answers it and no one else
    let reading = ChainData {
        block: 19_000_000,
        base_fee_wei_per_gas: 1_000_000_000_u64.into(),
        priority_fee_wei_per_gas: 100_000_000_u64.into(),
    };
    let stranger = Principal::from_slice(&[9; 29]);
    assert!(test_put_reading(&pic, canister, admin, 8453, &reading)
        .is_err_and(|why| why.contains("CallerNotRole")));
    assert_eq!(
        test_put_reading(&pic, canister, watcher(), 8453, &reading),
        Ok(())
    );

    let redacted = Config {
        rpc_urls: BTreeMap::from([(1, "***".to_string())]),
        ..config
    };
    let logged: Vec<EventType> = events_page(&pic, canister, stranger, 0, 100)
        .into_iter()
        .map(|event| event.payload)
        .collect();
    assert_eq!(
        logged,
        vec![
            EventType::ConfigChanged {
                json: serde_json::to_string(&redacted).unwrap()
            },
            EventType::RolesChanged {
                quoter: quoter().to_text(),
                watcher: watcher().to_text(),
            },
        ]
    );
}
