use super::*;

const SECRET: &str = "https://eth-mainnet.g.alchemy.com/v2/secret-key";

/// What a real deploy sends: two provider urls, both of them secrets.
fn secret_bearing() -> Config {
    Config {
        platform_fee_bps: 10,
        rpc_urls: BTreeMap::from([
            (1, SECRET.to_string()),
            (8453, "https://base.example/rpc?key=hunter2".to_string()),
        ]),
        // a vault address is public on-chain data, not a secret: it must survive
        vault_addresses: BTreeMap::from([(1, "0xvault".to_string())]),
        ecdsa_key_name: "key_1".to_string(),
        ..Config::default()
    }
}

/// An rpc url embeds its api key, so the public view must blank the values and keep
/// the chain ids, and must touch nothing else.
#[test]
fn redacted_blanks_rpc_urls_and_nothing_else() {
    let full = secret_bearing();
    let public = Config::from(types::Config::try_from(full.clone()).unwrap());

    assert_eq!(
        public.rpc_urls,
        BTreeMap::from([(1, "***".to_string()), (8453, "***".to_string())]),
        "every value blanked, every chain id kept"
    );
    // put the original urls back: if that restores the whole record, nothing else moved
    assert_eq!(
        Config {
            rpc_urls: full.rpc_urls.clone(),
            ..public
        },
        full
    );
}

/// The full config crosses the wire in `get_config_full` and `set_config`, so a stray debug
/// print of either must not show a key: every url blanks, every chain id and every other
/// knob stays.
#[test]
fn debug_never_prints_an_rpc_url() {
    let full = secret_bearing();
    let shown = format!("{full:?} {full:#?}");
    assert!(!shown.contains(SECRET), "leaked: {shown}");
    assert!(!shown.contains("hunter2"), "leaked: {shown}");
    assert!(!shown.contains("https://"), "leaked: {shown}");
    assert!(
        shown.contains("rpc_urls: {1: \"***\", 8453: \"***\"}"),
        "{shown}"
    );
    assert!(
        shown.contains("0xvault")
            && shown.contains("key_1")
            && shown.contains("platform_fee_bps: 10")
    );
}

/// The ops view carries the urls exactly as they were written.
#[test]
fn unredacted_round_trips_the_whole_config() {
    let full = secret_bearing();
    let domain = types::Config::try_from(full.clone()).unwrap();
    assert_eq!(Config::unredacted(domain), full);
}

/// The read-modify-write footgun: writing the public view back would brick rpc access.
#[test]
fn validate_rejects_the_redacted_placeholder_as_an_rpc_url() {
    let public = Config::from(types::Config::try_from(secret_bearing()).unwrap());
    let err = types::Config::try_from(public).unwrap_err();
    assert_eq!(
        err,
        types::ConfigError::RedactedRpcUrl {
            chain: ChainId::ETHEREUM
        }
    );
    assert!(err.to_string().contains("rpc_urls"), "{err}");
}

#[test]
fn every_duration_keeps_its_unit() {
    let wire = Config {
        quote_ttl_s: 46,
        batch_window_ms: 1_500,
        decision_timeout_min: 31,
        ..Config::default()
    };
    let domain = types::Config::try_from(wire.clone()).unwrap();
    assert_eq!(domain.quote_ttl, Duration::from_secs(46));
    assert_eq!(domain.batch_window, Duration::from_millis(1_500));
    assert_eq!(domain.decision_timeout, Duration::from_secs(31 * 60));
    assert_eq!(Config::unredacted(domain), wire);
}

#[test]
fn a_timeout_too_long_for_a_duration_is_refused() {
    let wire = Config {
        decision_timeout_min: u64::MAX / 60 + 1,
        ..Config::default()
    };
    assert_eq!(
        types::Config::try_from(wire),
        Err(types::ConfigError::DurationTooLong {
            field: "decision_timeout_min"
        })
    );
}

#[test]
fn a_vault_address_over_the_cap_is_refused() {
    let wire = Config {
        vault_addresses: BTreeMap::from([(8453, "a".repeat(257))]),
        ..Config::default()
    };
    assert_eq!(
        types::Config::try_from(wire),
        Err(types::ConfigError::VaultAddressTooLong {
            chain: ChainId::BASE,
            len: 257
        })
    );
}

#[test]
fn a_config_error_names_its_knob_on_the_wire() {
    let redacted = Config::from(types::Config::try_from(secret_bearing()).unwrap());
    let err = types::Config::try_from(redacted).unwrap_err();
    assert_eq!(
        ConfigError::from(err),
        ConfigError::RedactedRpcUrl { chain_id: 1 }
    );
    let incoherent = types::Config {
        platform_fee: types::BasisPoints::new(40),
        ..types::Config::default()
    };
    assert_eq!(
        ConfigError::from(incoherent.validate().unwrap_err()),
        ConfigError::PlatformFeeAboveMax {
            platform_fee_bps: 40,
            max_fee_bps: 30
        }
    );
    let empty_vault = Config {
        vault_addresses: BTreeMap::from([(8453, String::new())]),
        ..Config::default()
    };
    let err = types::Config::try_from(empty_vault)
        .unwrap()
        .validate()
        .unwrap_err();
    assert_eq!(
        ConfigError::from(err),
        ConfigError::EmptyVaultAddress { chain_id: 8453 }
    );
}
