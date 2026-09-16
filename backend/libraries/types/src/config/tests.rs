use super::*;

/// The defaults are spec numbers, not preferences: this test is the spec.
#[test]
fn defaults_match_spec() {
    let c = Config::default();
    assert_eq!(c.platform_fee, BasisPoints::new(0));
    assert_eq!(c.max_fee, BasisPoints::new(30));
    assert_eq!(c.max_swap, UsdAmount::new(1_000));
    assert_eq!(c.quote_ttl, Duration::from_secs(45));
    assert_eq!(c.permit_deadline, Duration::from_secs(120));
    assert_eq!(c.chain_data_max_age, Duration::from_secs(10));
    assert_eq!(c.batch_window, Duration::from_millis(2_000));
    assert_eq!(c.max_batch_items, 10);
    assert_eq!(c.decision_timeout, Duration::from_secs(30 * 60));
    assert_eq!(c.rail_status_max_age, Duration::from_secs(30));
    assert!(!c.simulate_before_sign);
    assert_eq!(c.expiry_check_interval, Duration::from_secs(60));
    assert_eq!(c.replay_audit_interval, Duration::from_secs(21_600));
    assert_eq!(
        c.confirmations,
        BTreeMap::from(
            [(1, 1), (8453, 1), (56, 1), (137, 6), (42161, 1)]
                .map(|(chain, depth)| (ChainId::new(chain), BlockDepth::new(depth)))
        )
    );
    // deploy-time values, empty in code
    assert!(c.rpc_urls.is_empty());
    assert!(c.vault_addresses.is_empty());
    assert_eq!(c.ecdsa_key_name, "dfx_test_key");
}

const SECRET: &str = "https://eth-mainnet.g.alchemy.com/v2/secret-key";

/// What a real deploy looks like: two provider urls, both of them secrets.
fn secret_bearing() -> Config {
    Config {
        platform_fee: BasisPoints::new(10),
        rpc_urls: BTreeMap::from([
            (ChainId::ETHEREUM, SECRET.parse().unwrap()),
            (
                ChainId::BASE,
                "https://base.example/rpc?key=hunter2".parse().unwrap(),
            ),
        ]),
        // a vault address is public on-chain data, not a secret: it must survive
        vault_addresses: BTreeMap::from([(ChainId::ETHEREUM, "0xvault".parse().unwrap())]),
        ecdsa_key_name: "key_1".to_string(),
        ..Config::default()
    }
}

/// A stray `{:?}` anywhere would print the keys if the urls were plain text, so the
/// config's Debug, which the log records, blanks them by type.
#[test]
fn debug_never_prints_an_rpc_url() {
    let shown = format!("{:?}", secret_bearing());
    assert!(
        !shown.contains(SECRET),
        "a stray debug print leaked: {shown}"
    );
    assert!(
        !shown.contains("hunter2"),
        "a stray debug print leaked: {shown}"
    );
    assert!(shown.contains("***"));
    // everything that is not a secret still prints
    assert!(shown.contains("0xvault") && shown.contains("key_1"));
}

#[test]
fn defaults_are_valid() {
    Config::default()
        .validate()
        .expect("the defaults are valid");
}

#[test]
fn validate_rejects_a_ceiling_above_one_hundred_percent() {
    let config = Config {
        max_fee: BasisPoints::new(10_001),
        ..Config::default()
    };
    assert_eq!(
        config.validate(),
        Err(ConfigError::MaxFeeAboveHundredPercent(BasisPoints::new(
            10_001
        )))
    );
}

#[test]
fn validate_rejects_a_platform_fee_above_the_ceiling() {
    let config = Config {
        platform_fee: BasisPoints::new(31),
        max_fee: BasisPoints::new(30),
        ..Config::default()
    };
    let err = config.validate().unwrap_err();
    assert_eq!(
        err,
        ConfigError::PlatformFeeAboveMax {
            platform_fee: BasisPoints::new(31),
            max_fee: BasisPoints::new(30)
        }
    );
    assert!(err.to_string().contains("platform_fee_bps"), "{err}");
}

#[test]
fn validate_rejects_an_empty_ecdsa_key_name() {
    let config = Config {
        ecdsa_key_name: String::new(),
        ..Config::default()
    };
    assert_eq!(config.validate(), Err(ConfigError::EmptyEcdsaKeyName));
}

#[test]
fn interval_changes_flags_each_timer_for_its_own_interval_only() {
    let base = Config::default();
    let changes = |new: Config| {
        let c = base.interval_changes(&new);
        (c.expiry, c.audit)
    };
    let fee_only = Config {
        platform_fee: BasisPoints::new(10),
        ..Config::default()
    };
    assert_eq!(changes(fee_only), (false, false));
    let expiry = Config {
        expiry_check_interval: Duration::from_secs(61),
        ..Config::default()
    };
    assert_eq!(changes(expiry), (true, false));
    let audit = Config {
        replay_audit_interval: Duration::from_secs(21_601),
        ..Config::default()
    };
    assert_eq!(changes(audit), (false, true));
}

#[test]
fn validate_rejects_a_timer_interval_above_a_year() {
    let over = MAX_TIMER_INTERVAL + Duration::from_secs(1);
    assert_eq!(
        Config {
            expiry_check_interval: over,
            ..Config::default()
        }
        .validate(),
        Err(ConfigError::TimerIntervalTooLong {
            field: "expiry_check_interval_s",
            interval: over
        })
    );
    assert_eq!(
        Config {
            replay_audit_interval: over,
            ..Config::default()
        }
        .validate(),
        Err(ConfigError::TimerIntervalTooLong {
            field: "replay_audit_interval_s",
            interval: over
        })
    );
    Config {
        expiry_check_interval: MAX_TIMER_INTERVAL,
        replay_audit_interval: MAX_TIMER_INTERVAL,
        ..Config::default()
    }
    .validate()
    .expect("a year exactly is allowed");
}

/// The stored copy is the operative one, so it must keep the real urls.
#[test]
fn storage_keeps_the_secrets() {
    let config = secret_bearing();
    let back = Config::from_bytes(config.to_bytes());
    assert_eq!(back, config);
    assert_eq!(back.rpc_urls[&ChainId::ETHEREUM].expose(), SECRET);
}
