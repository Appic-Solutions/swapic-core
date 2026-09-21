use super::*;
use ic_stable_structures::Storable;
use std::borrow::Cow;

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
    assert_eq!(c.max_refunds_per_sweep, RefundsPerSweep::new(50));
    assert_eq!(c.max_evictions_per_sweep, EvictionsPerSweep::new(200));
    assert_eq!(c.audit_chunk_events, AuditChunk::new(1_000));
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

/// The sweep interval has a floor as well as a ceiling: a zero interval is clamped to a
/// second rather than refused, so a config could otherwise put a pass that reads stable
/// memory and appends on every second.
#[test]
fn validate_rejects_a_sweep_interval_below_the_floor() {
    let under = MIN_EXPIRY_CHECK_INTERVAL - Duration::from_secs(1);
    for interval in [Duration::ZERO, Duration::from_secs(1), under] {
        let err = Config {
            expiry_check_interval: interval,
            ..Config::default()
        }
        .validate()
        .unwrap_err();
        assert_eq!(
            err,
            ConfigError::TimerIntervalTooShort {
                field: "expiry_check_interval_s",
                interval,
                floor: MIN_EXPIRY_CHECK_INTERVAL
            }
        );
        assert!(err.to_string().contains("expiry_check_interval_s"), "{err}");
    }
    Config {
        expiry_check_interval: MIN_EXPIRY_CHECK_INTERVAL,
        ..Config::default()
    }
    .validate()
    .expect("the floor itself is allowed");
    // the floor is the sweep's alone: the audit timer keeps only its ceiling
    Config {
        replay_audit_interval: Duration::from_secs(1),
        ..Config::default()
    }
    .validate()
    .expect("the audit interval has no floor");
}

#[test]
fn validate_rejects_an_empty_vault_address() {
    let config = Config {
        vault_addresses: BTreeMap::from([
            (ChainId::ETHEREUM, "0xvault".parse().unwrap()),
            (ChainId::BASE, "".parse().unwrap()),
        ]),
        ..Config::default()
    };
    let err = config.validate().unwrap_err();
    assert_eq!(
        err,
        ConfigError::EmptyVaultAddress {
            chain: ChainId::BASE
        }
    );
    assert!(err.to_string().contains("vault_addresses[8453]"), "{err}");
}

#[test]
fn validate_rejects_an_empty_rpc_url() {
    let config = Config {
        rpc_urls: BTreeMap::from([
            (ChainId::ETHEREUM, SECRET.parse().unwrap()),
            (ChainId::ARBITRUM, "".parse().unwrap()),
        ]),
        ..Config::default()
    };
    let err = config.validate().unwrap_err();
    assert_eq!(
        err,
        ConfigError::EmptyRpcUrl {
            chain: ChainId::ARBITRUM
        }
    );
    assert!(err.to_string().contains("rpc_urls[42161]"), "{err}");
    secret_bearing()
        .validate()
        .expect("real urls and addresses pass");
}

/// Every duration knob is bounded, and each rejection names its field and its cap. A knob
/// near `u64::MAX` overflows the deadline it is added to, and the comparison that deadline
/// feeds then never fires: no pending quote is ever evicted, no wait ever times out.
#[test]
fn validate_rejects_every_duration_knob_above_its_cap() {
    /// One duration knob: its wire name, its cap, and a config with that knob set.
    type Knob = (&'static str, Duration, fn(Duration) -> Config);
    let knobs: [Knob; 6] = [
        ("quote_ttl_s", MAX_SHORT_DURATION, |d| Config {
            quote_ttl: d,
            ..Config::default()
        }),
        ("permit_deadline_s", MAX_SHORT_DURATION, |d| Config {
            permit_deadline: d,
            ..Config::default()
        }),
        ("chain_data_max_age_s", MAX_SHORT_DURATION, |d| Config {
            chain_data_max_age: d,
            ..Config::default()
        }),
        ("decision_timeout_min", MAX_SHORT_DURATION, |d| Config {
            decision_timeout: d,
            ..Config::default()
        }),
        ("rail_status_max_age_s", MAX_SHORT_DURATION, |d| Config {
            rail_status_max_age: d,
            ..Config::default()
        }),
        // a batching window is the one knob measured in milliseconds, and a minute of it is
        // already far more than a batch waits for
        ("batch_window_ms", MAX_BATCH_WINDOW, |d| Config {
            batch_window: d,
            ..Config::default()
        }),
    ];
    for (field, cap, with) in knobs {
        let over = cap + Duration::from_secs(1);
        let err = with(over).validate().unwrap_err();
        assert_eq!(
            err,
            ConfigError::DurationAboveCap {
                field,
                duration: over,
                cap
            }
        );
        assert!(err.to_string().contains(field), "{err}");
        with(cap).validate().expect("the cap itself is allowed");
        // the near-overflow shape the bound exists for
        assert!(with(Duration::from_secs(u64::MAX)).validate().is_err());
    }
}

/// A zero cap makes no progress at all and an outsized one is the unbounded batch the cap
/// exists to prevent, so both are refused by name.
#[test]
fn validate_rejects_a_cap_outside_its_range() {
    let refunds = |cap| Config {
        max_refunds_per_sweep: RefundsPerSweep::new(cap),
        ..Config::default()
    };
    let evictions = |cap| Config {
        max_evictions_per_sweep: EvictionsPerSweep::new(cap),
        ..Config::default()
    };
    assert_eq!(
        refunds(0).validate(),
        Err(ConfigError::CapOutOfRange {
            field: "max_refunds_per_sweep",
            cap: 0,
            ceiling: RefundsPerSweep::CEILING
        })
    );
    let over = RefundsPerSweep::CEILING + 1;
    assert_eq!(
        refunds(over).validate(),
        Err(ConfigError::CapOutOfRange {
            field: "max_refunds_per_sweep",
            cap: over,
            ceiling: RefundsPerSweep::CEILING
        })
    );
    refunds(RefundsPerSweep::CEILING)
        .validate()
        .expect("the ceiling itself is allowed");
    refunds(1).validate().expect("one item a pass is allowed");

    let chunk = |cap| Config {
        audit_chunk_events: AuditChunk::new(cap),
        ..Config::default()
    };
    assert!(chunk(0).validate().is_err());
    assert!(chunk(AuditChunk::CEILING + 1).validate().is_err());
    chunk(AuditChunk::CEILING)
        .validate()
        .expect("the ceiling itself is allowed");

    let err = evictions(0).validate().unwrap_err();
    assert_eq!(
        err,
        ConfigError::CapOutOfRange {
            field: "max_evictions_per_sweep",
            cap: 0,
            ceiling: EvictionsPerSweep::CEILING
        }
    );
    assert!(err.to_string().contains("max_evictions_per_sweep"), "{err}");
    assert!(evictions(EvictionsPerSweep::CEILING + 1)
        .validate()
        .is_err());
    evictions(EvictionsPerSweep::CEILING)
        .validate()
        .expect("the ceiling itself is allowed");
}

/// A cap holding its default is absent from the stored bytes, which is what keeps the
/// stored layout append-only: a config a wasm without these knobs wrote decodes here, and
/// reads the defaults. The shortest encoding is the one the golden file pins.
#[test]
fn a_cap_at_its_default_is_absent_from_storage_and_reads_back_as_the_default() {
    let defaults = Config::default().to_bytes().into_owned();
    // 0x91: an array of the seventeen fields that existed before the caps did
    assert_eq!(defaults[0], 0x91, "a default cap writes nothing of its own");
    assert_eq!(
        Config::from_bytes(Cow::Owned(defaults.clone())),
        Config::default(),
        "so bytes without the caps read as the defaults"
    );

    // the interior case: a later cap set, an earlier one at its default
    let mixed = Config {
        max_evictions_per_sweep: EvictionsPerSweep::new(7),
        ..Config::default()
    };
    let bytes = mixed.to_bytes().into_owned();
    assert!(bytes.len() > defaults.len());
    assert_eq!(Config::from_bytes(Cow::Owned(bytes)), mixed);

    let both = Config {
        max_refunds_per_sweep: RefundsPerSweep::new(9),
        max_evictions_per_sweep: EvictionsPerSweep::new(7),
        ..Config::default()
    };
    assert_eq!(Config::from_bytes(both.to_bytes()), both);
}

/// The stored copy is the operative one, so it must keep the real urls.
#[test]
fn storage_keeps_the_secrets() {
    let config = secret_bearing();
    let back = Config::from_bytes(config.to_bytes());
    assert_eq!(back, config);
    assert_eq!(back.rpc_urls[&ChainId::ETHEREUM].expose(), SECRET);
}
