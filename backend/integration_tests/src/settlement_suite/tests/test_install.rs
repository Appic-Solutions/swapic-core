use crate::client::settlement::{
    event_count, events_page, get_config_full, get_pending, register_quote, verify_chain,
    verify_replay,
};
use crate::settlement_suite::init::{empty_canister, init_arg, quoter, watcher};
use crate::wasms;
use candid::{encode_one, Principal};
use pocket_ic::PocketIc;
use settlement_api::types::config::Config;
use settlement_api::types::errors::{GuardError, RegisterQuoteError, Role};
use settlement_api::types::events::EventType;
use settlement_api::types::init::InitArg;
use settlement_api::types::quote::{GasMode, Quote};
use std::collections::BTreeMap;
use std::time::Duration;

fn install(pic: &PocketIc, canister: Principal, admin: Principal, arg: &InitArg) {
    pic.install_canister(
        canister,
        wasms::settlement(),
        encode_one(arg).unwrap(),
        Some(admin),
    );
}

/// An install the canister refuses. `install_canister` panics on a refusal, so this goes
/// through the fallible reinstall, which on a canister with no code is an install.
fn refused_install(pic: &PocketIc, canister: Principal, admin: Principal, arg: &InitArg) -> String {
    pic.reinstall_canister(
        canister,
        wasms::settlement(),
        encode_one(arg).unwrap(),
        Some(admin),
    )
    .expect_err("the install is refused")
    .reject_message
}

fn quote_live_at(pic: &PocketIc) -> Quote {
    let now_s = pic.get_time().as_nanos_since_unix_epoch() / 1_000_000_000;
    Quote {
        version: 1,
        src_chain: 8453,
        src_token: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".into(),
        amount_in: 25_000_000_u32.into(),
        dst_chain: 42161,
        dst_token: "0xaf88d065e77c8cC2239327C5EDb3A432268e5831".into(),
        expected_out: 24_990_000_u32.into(),
        min_out: 24_900_000_u32.into(),
        dst_address: "0x7551A66653f9a20979ed81835a0b7008EC83401b".into(),
        refund_address: None,
        auto_refund: true,
        gas_mode: GasMode::Gasless,
        rail: "cctp_v2_fast".into(),
        expires_at_s: now_s + 60,
        nonce: 1,
    }
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
    let incoherent_fee = InitArg {
        config: Config {
            platform_fee_bps: 40,
            max_fee_bps: 30,
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
    let empty_vault = InitArg {
        config: Config {
            vault_addresses: BTreeMap::from([(8453, String::new())]),
            ..Config::default()
        },
        ..init_arg()
    };
    let anonymous_quoter = InitArg {
        quoter: Principal::anonymous(),
        ..init_arg()
    };
    for (arg, why) in [
        (incoherent_fee, "platform_fee_bps"),
        (redacted_url, "rpc_urls[1]"),
        (empty_vault, "vault_addresses[8453]"),
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
    // the refused install's instructions count against the next one for a few minutes
    pic.advance_time(Duration::from_secs(600));
    pic.tick();
    install(&pic, canister, admin, &init_arg());
    assert_eq!(event_count(&pic, canister, admin), 2);
}

/// A fresh install is in service at once: the arg's config is the stored one, its quoter
/// and watcher hold the roles, and the log opens with the two audit lines that record them.
#[test]
fn a_fresh_install_has_the_args_config_roles_and_audit_events() {
    let (pic, canister, admin) = empty_canister();
    let config = Config {
        platform_fee_bps: 10,
        rpc_urls: BTreeMap::from([(1, "https://eth.example/v2/secret-key".to_string())]),
        vault_addresses: BTreeMap::from([(8453, "0xvault".to_string())]),
        ecdsa_key_name: "key_1".to_string(),
        ..Config::default()
    };
    let arg = InitArg {
        config: config.clone(),
        ..init_arg()
    };
    install(&pic, canister, admin, &arg);

    assert_eq!(get_config_full(&pic, canister, admin).unwrap(), config);

    let quote = quote_live_at(&pic);
    let stranger = Principal::from_slice(&[9; 29]);
    assert_eq!(
        register_quote(&pic, canister, admin, &quote),
        Err(RegisterQuoteError::Guard(GuardError::CallerNotRole(
            Role::Quoter
        )))
    );
    let hash = register_quote(&pic, canister, quoter(), &quote).expect("the arg's quoter");
    assert_eq!(
        get_pending(&pic, canister, watcher(), hash),
        Ok(Some(quote)),
        "and the arg's watcher reads"
    );
    assert_eq!(
        get_pending(&pic, canister, stranger, hash),
        Err(GuardError::CallerNotQuoterOrWatcher)
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
    assert!(verify_chain(&pic, canister, stranger));
    assert!(verify_replay(&pic, canister, stranger));
}
