use super::*;

/// The defaults are spec numbers, not preferences: this test is the spec.
#[test]
fn defaults_match_spec() {
    let c = Config::default();
    assert_eq!(c.platform_fee_bps, 0);
    assert_eq!(c.max_fee_bps, 30);
    assert_eq!(c.max_swap_usd, 1_000);
    assert_eq!(c.quote_ttl_s, 45);
    assert_eq!(c.permit_deadline_s, 120);
    assert_eq!(c.chain_data_max_age_s, 10);
    assert_eq!(c.batch_window_ms, 2_000);
    assert_eq!(c.max_batch_items, 10);
    assert_eq!(c.decision_timeout_min, 30);
    assert_eq!(c.rail_status_max_age_s, 30);
    assert!(!c.simulate_before_sign);
    assert_eq!(c.expiry_check_interval_s, 60);
    assert_eq!(c.replay_audit_interval_s, 21_600);
    assert_eq!(
        c.confirmations,
        BTreeMap::from([(1, 1), (8453, 1), (56, 1), (137, 6), (42161, 1)])
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
    let public = full.redacted();

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

/// `redacted()` guards the api surface, but a stray `{:?}` anywhere would still print
/// the keys, so Debug blanks them too.
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
    // the two blanking paths must agree, or the logged event json would drift
    assert_eq!(shown, format!("{:?}", secret_bearing().redacted()));
    // everything that is not a secret still prints
    assert!(shown.contains("0xvault") && shown.contains("key_1"));
}

#[test]
fn defaults_are_valid() {
    Config::default()
        .validate()
        .expect("the defaults are valid");
}

fn rejects(config: Config, field: &str) {
    let err = config
        .validate()
        .expect_err("validate should reject this config");
    assert!(err.contains(field), "the error must name {field}: {err}");
}

#[test]
fn validate_rejects_a_ceiling_above_one_hundred_percent() {
    rejects(
        Config {
            max_fee_bps: 10_001,
            ..Config::default()
        },
        "max_fee_bps",
    );
}

#[test]
fn validate_rejects_a_platform_fee_above_the_ceiling() {
    rejects(
        Config {
            platform_fee_bps: 31,
            max_fee_bps: 30,
            ..Config::default()
        },
        "platform_fee_bps",
    );
}

#[test]
fn validate_rejects_an_empty_ecdsa_key_name() {
    rejects(
        Config {
            ecdsa_key_name: String::new(),
            ..Config::default()
        },
        "ecdsa_key_name",
    );
}

/// The read-modify-write footgun: writing the public view back would brick rpc access.
#[test]
fn validate_rejects_the_redacted_placeholder_as_an_rpc_url() {
    rejects(secret_bearing().redacted(), "rpc_urls");
}

#[test]
fn interval_changes_flags_each_timer_for_its_own_interval_only() {
    let base = Config::default();
    let changes = |new: Config| {
        let c = base.interval_changes(&new);
        (c.expiry, c.audit)
    };
    let fee_only = Config {
        platform_fee_bps: 10,
        ..Config::default()
    };
    assert_eq!(changes(fee_only), (false, false));
    let expiry = Config {
        expiry_check_interval_s: 61,
        ..Config::default()
    };
    assert_eq!(changes(expiry), (true, false));
    let audit = Config {
        replay_audit_interval_s: 21_601,
        ..Config::default()
    };
    assert_eq!(changes(audit), (false, true));
}

#[test]
fn validate_rejects_a_timer_interval_above_a_year() {
    rejects(
        Config {
            expiry_check_interval_s: MAX_TIMER_INTERVAL_S + 1,
            ..Config::default()
        },
        "expiry_check_interval_s",
    );
    rejects(
        Config {
            replay_audit_interval_s: MAX_TIMER_INTERVAL_S + 1,
            ..Config::default()
        },
        "replay_audit_interval_s",
    );
    Config {
        expiry_check_interval_s: MAX_TIMER_INTERVAL_S,
        replay_audit_interval_s: MAX_TIMER_INTERVAL_S,
        ..Config::default()
    }
    .validate()
    .expect("a year exactly is allowed");
}
