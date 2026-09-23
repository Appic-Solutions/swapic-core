use super::*;
use crate::evm::EvmAddress;
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
    // Ethereum's depth is the floor a reorg on it makes necessary, not the spec's one
    assert_eq!(
        c.confirmations,
        BTreeMap::from(
            [(1, 12), (8453, 1), (56, 1), (137, 6), (42161, 1)]
                .map(|(chain, depth)| (ChainId::new(chain), BlockDepth::new(depth)))
        )
    );
    // a day of the chain whose blocks come fastest, so a deposit made while the canister
    // was halted is still in the range a claim reads
    assert_eq!(c.deposit_lookback_blocks, DepositLookback::new(345_600));
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

/// A depth decides money on both sides, and Ethereum mainnet reorgs routinely: a config
/// that would claim a deposit or close a burn there at less than the floor is refused,
/// whatever else it holds. Every other chain is the deploy's own call.
#[test]
fn validate_rejects_a_mainnet_depth_below_the_floor() {
    for depth in [1, MIN_ETHEREUM_CONFIRMATIONS.get() - 1] {
        let shallow = Config {
            confirmations: BTreeMap::from([(ChainId::ETHEREUM, BlockDepth::new(depth))]),
            ..Config::default()
        };
        assert_eq!(
            shallow.validate(),
            Err(ConfigError::DepthTooShallow {
                chain: ChainId::ETHEREUM,
                depth: BlockDepth::new(depth),
                floor: MIN_ETHEREUM_CONFIRMATIONS,
            })
        );
    }
    let at_the_floor = Config {
        confirmations: BTreeMap::from([(ChainId::ETHEREUM, MIN_ETHEREUM_CONFIRMATIONS)]),
        ..Config::default()
    };
    assert_eq!(at_the_floor.validate(), Ok(()));
    // another chain's depth is the deploy's to size, and a chain with no depth at all is
    // refused where money is decided, not here
    let shallow_elsewhere = Config {
        confirmations: BTreeMap::from([(ChainId::ARBITRUM, BlockDepth::new(1))]),
        ..Config::default()
    };
    assert_eq!(shallow_elsewhere.validate(), Ok(()));
    assert_eq!(Config::default().validate(), Ok(()));
}

#[test]
fn interval_changes_flags_each_timer_for_its_own_interval_only() {
    let base = Config::default();
    let changes = |new: Config| {
        let c = base.interval_changes(&new);
        (c.expiry, c.audit, c.rail_status)
    };
    let fee_only = Config {
        platform_fee: BasisPoints::new(10),
        ..Config::default()
    };
    assert_eq!(changes(fee_only), (false, false, false));
    let expiry = Config {
        expiry_check_interval: Duration::from_secs(61),
        ..Config::default()
    };
    assert_eq!(changes(expiry), (true, false, false));
    let audit = Config {
        replay_audit_interval: Duration::from_secs(21_601),
        ..Config::default()
    };
    assert_eq!(changes(audit), (false, true, false));
    let rail_status = Config {
        rail_status_max_age: Duration::from_secs(31),
        ..Config::default()
    };
    assert_eq!(changes(rail_status), (false, false, true));
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

/// The other way a cap reads back as its default: a null where the knob sits, which is what
/// an encoder that keeps every index writes for a knob it does not set. The golden file
/// cannot pin this shape, because a golden line must encode back to itself and a cap at its
/// default encodes as its value, so the decode is checked here instead.
#[test]
fn a_cap_stored_as_null_reads_back_as_the_default() {
    let mixed = Config {
        max_evictions_per_sweep: EvictionsPerSweep::new(7),
        ..Config::default()
    };
    let mut bytes = mixed.to_bytes().into_owned();
    // the two elements the record ends with: the refund cap at its default, written out
    // because a later knob is set, then the eviction cap
    let tail = bytes.split_off(bytes.len() - 3);
    assert_eq!(
        tail,
        [0x18, RefundsPerSweep::DEFAULT.get() as u8, 0x07],
        "the stored shape this test rewrites"
    );
    // the same record with that knob absent rather than spelled out
    bytes.extend_from_slice(&[0xf6, 0x07]);
    assert_eq!(
        Config::from_bytes(Cow::Owned(bytes)),
        mixed,
        "a null where a cap sits is that cap's default"
    );
}

/// The stored copy is the operative one, so it must keep the real urls.
#[test]
fn storage_keeps_the_secrets() {
    let config = secret_bearing();
    let back = Config::from_bytes(config.to_bytes());
    assert_eq!(back, config);
    assert_eq!(back.rpc_urls[&ChainId::ETHEREUM].expose(), SECRET);
}

/// A duration knob is refused with the value it was set to, not a rounding of it: a
/// batching window of 60.5s is half a second over its cap, and the error says so.
#[test]
fn a_duration_error_keeps_its_milliseconds() {
    let err = Config {
        batch_window: Duration::from_millis(60_500),
        ..Config::default()
    }
    .validate()
    .unwrap_err();
    assert_eq!(
        err,
        ConfigError::DurationAboveCap {
            field: "batch_window_ms",
            duration: Duration::from_millis(60_500),
            cap: MAX_BATCH_WINDOW
        }
    );
    assert_eq!(
        err.to_string(),
        "batch_window_ms is 60.5s, above the cap of 60s"
    );
}

/// A batch of nothing is no batch at all, and an unbounded one is the trap the caps exist
/// to prevent, so the batch size is held to the same rule as the per-pass caps.
#[test]
fn validate_rejects_a_batch_size_outside_its_range() {
    let batch = |items| Config {
        max_batch_items: items,
        ..Config::default()
    };
    assert_eq!(
        batch(0).validate(),
        Err(ConfigError::CapOutOfRange {
            field: "max_batch_items",
            cap: 0,
            ceiling: MAX_BATCH_ITEMS
        })
    );
    let over = MAX_BATCH_ITEMS + 1;
    assert_eq!(
        batch(over).validate(),
        Err(ConfigError::CapOutOfRange {
            field: "max_batch_items",
            cap: over,
            ceiling: MAX_BATCH_ITEMS
        })
    );
    batch(MAX_BATCH_ITEMS)
        .validate()
        .expect("the ceiling itself is allowed");
    batch(1).validate().expect("one item a batch is allowed");
}

/// The deposit read looks back a bounded number of blocks from the chain's head for the
/// user's deposit. A lookback of nothing finds nothing, and an unbounded one is a range no
/// provider serves, so the knob is a cap like the per-pass ones: one to its ceiling, at its
/// default absent from storage so the stored config's bytes do not move.
#[test]
fn the_deposit_lookback_is_a_cap_with_the_defaults_shape() {
    // a day of the chain whose blocks come fastest, which is a day on every slower chain
    // many times over
    assert_eq!(
        Config::default().deposit_lookback_blocks,
        DepositLookback::new(345_600)
    );
    let lookback = |blocks| Config {
        deposit_lookback_blocks: DepositLookback::new(blocks),
        ..Config::default()
    };
    assert_eq!(
        lookback(0).validate(),
        Err(ConfigError::CapOutOfRange {
            field: "deposit_lookback_blocks",
            cap: 0,
            ceiling: DepositLookback::CEILING
        })
    );
    let over = DepositLookback::CEILING + 1;
    assert_eq!(
        lookback(over).validate(),
        Err(ConfigError::CapOutOfRange {
            field: "deposit_lookback_blocks",
            cap: over,
            ceiling: DepositLookback::CEILING
        })
    );
    lookback(DepositLookback::CEILING)
        .validate()
        .expect("the ceiling itself is allowed");
    lookback(1).validate().expect("one block back is allowed");

    // at its default the knob writes nothing, so the stored bytes are the ones the golden
    // file already pins; set, it is the twenty-first element
    let defaults = Config::default().to_bytes().into_owned();
    assert_eq!(
        defaults[0], 0x91,
        "a default lookback writes nothing of its own"
    );
    let set = lookback(7);
    let bytes = set.to_bytes().into_owned();
    assert_eq!(
        bytes[0], 0x95,
        "a set lookback is written after the three sweep caps"
    );
    assert_eq!(Config::from_bytes(Cow::Owned(bytes)), set);
}

/// The lookback's ceiling is the widest range the deposit read walks, its window cap times
/// its window, so no config the door accepts makes the read refuse every range as too
/// wide (every claim and every arrival check would fail from then on). The old ceiling of
/// a million blocks is refused by name, the walk's own width is accepted, and one block
/// more is refused.
#[test]
fn the_lookback_ceiling_is_the_range_the_read_can_walk() {
    let lookback = |blocks| Config {
        deposit_lookback_blocks: DepositLookback::new(blocks),
        ..Config::default()
    };
    assert_eq!(
        lookback(1_000_000).validate(),
        Err(ConfigError::CapOutOfRange {
            field: "deposit_lookback_blocks",
            cap: 1_000_000,
            ceiling: 400_000,
        }),
        "the ceiling the read could never walk"
    );
    lookback(400_000)
        .validate()
        .expect("forty windows of ten thousand blocks is the walk's own width");
    assert_eq!(
        lookback(400_001).validate(),
        Err(ConfigError::CapOutOfRange {
            field: "deposit_lookback_blocks",
            cap: 400_001,
            ceiling: 400_000,
        })
    );
    assert_eq!(
        u64::from(DepositLookback::CEILING),
        MAX_LOGS_WINDOWS * LOGS_WINDOW_BLOCKS,
        "the ceiling is spelled from the walk's own constants"
    );
}

/// The rail knobs are deploy-time facts like the vault addresses: per-chain tables and
/// three contract addresses. Unset, they are absent from the stored config, so a config
/// written before they existed reads back with them empty and the golden lines do not
/// move; set, they round-trip, tables by chain.
#[test]
fn the_rail_knobs_are_absent_while_unset_and_round_trip_when_set() {
    let defaults = Config::default();
    assert!(defaults.cctp_domains.0.is_empty());
    assert!(defaults.usdc_addresses.0.is_empty());
    assert_eq!(defaults.token_messenger, None);
    assert_eq!(defaults.message_transmitter, None);
    assert_eq!(defaults.eco_portal, None);
    assert_eq!(
        defaults.to_bytes()[0],
        0x91,
        "unset rail knobs write nothing of their own"
    );

    let usdc: EvmAddress = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"
        .parse()
        .unwrap();
    let messenger: EvmAddress = "0x28b5a0e9C621a5BadaA536219b3a228C8168cf5d"
        .parse()
        .unwrap();
    let wired = Config {
        cctp_domains: ChainTable(BTreeMap::from([
            (ChainId::ETHEREUM, CctpDomain::new(0)),
            (ChainId::BASE, CctpDomain::new(6)),
            (ChainId::ARBITRUM, CctpDomain::new(3)),
        ])),
        usdc_addresses: ChainTable(BTreeMap::from([(ChainId::BASE, usdc)])),
        token_messenger: Some(messenger),
        ..Config::default()
    };
    assert_eq!(Config::from_bytes(wired.to_bytes()), wired);
    assert_eq!(
        wired.cctp_domains.get(ChainId::BASE),
        Some(CctpDomain::new(6))
    );
    assert_eq!(wired.cctp_domains.get(ChainId::POLYGON), None);
    assert_eq!(wired.usdc_addresses.get(ChainId::BASE), Some(usdc));

    // the last knob set: everything before it is written, the ones after it are not
    let portal_only = Config {
        eco_portal: Some(messenger),
        ..Config::default()
    };
    assert_eq!(Config::from_bytes(portal_only.to_bytes()), portal_only);
    assert_eq!(
        portal_only.to_bytes()[..2],
        [0x98, 26],
        "twenty-six fields up to the portal, a length CBOR writes in its own byte"
    );
    // a table written as null, by an encoder that keeps every index, reads as empty
    let empty: ChainTable<CctpDomain> = minicbor::decode(&[0xf6]).unwrap();
    assert!(empty.0.is_empty());
}

/// The Eco rail is off until its route is designed, so the knob that turns it on is off by
/// default and writes nothing while it is: a config written before the knob existed reads
/// back with Eco off and the golden lines do not move. Turned on, it round-trips, and a
/// null where it sits reads as off.
#[test]
fn eco_is_off_by_default_and_absent_from_storage_while_it_is() {
    let defaults = Config::default();
    assert!(!defaults.eco_enabled.is_on());
    assert_eq!(
        defaults.to_bytes()[0],
        0x91,
        "the knob at its default writes nothing of its own"
    );
    let on = Config {
        eco_enabled: EcoEnabled::ON,
        ..Config::default()
    };
    assert_eq!(Config::from_bytes(on.to_bytes()), on);
    assert!(on.eco_enabled.is_on());
    assert_eq!(
        on.to_bytes()[..2],
        [0x98, 27],
        "twenty-seven fields up to the knob"
    );
    let off: EcoEnabled = minicbor::decode(&[0xf6]).unwrap();
    assert!(!off.is_on(), "a null where the knob sits is Eco off");
}

/// The grace a claim is admitted in after a quote's deposit deadline is an hour unless a
/// deploy sets it, and it writes nothing of its own at that default: a config written
/// before the knob existed reads back with the hour and the golden lines do not move. Set,
/// it round-trips, and a null where it sits reads as the default.
#[test]
fn the_claim_grace_is_an_hour_by_default_and_absent_from_storage_while_it_is() {
    let defaults = Config::default();
    assert_eq!(defaults.claim_grace.get(), Duration::from_secs(3_600));
    assert_eq!(
        defaults.to_bytes()[0],
        0x91,
        "the knob at its default writes nothing of its own"
    );
    let set = Config {
        claim_grace: ClaimGrace::new(Duration::from_secs(900)),
        ..Config::default()
    };
    assert_eq!(Config::from_bytes(set.to_bytes()), set);
    assert_eq!(
        set.to_bytes()[..2],
        [0x98, 28],
        "twenty-eight fields up to the knob"
    );
    let null: ClaimGrace = minicbor::decode(&[0xf6]).unwrap();
    assert_eq!(
        null,
        ClaimGrace::DEFAULT,
        "a null where the knob sits is the hour"
    );
}

/// A grace shorter than a watcher restart strands the deposits the restart held up, and
/// one longer than a day keeps a quote in the pending store for longer than any window
/// inside one swap may be, so both are refused by name. The bounds themselves are allowed.
#[test]
fn validate_holds_the_claim_grace_between_ten_minutes_and_a_day() {
    let with = |secs| Config {
        claim_grace: ClaimGrace::new(Duration::from_secs(secs)),
        ..Config::default()
    };
    assert_eq!(
        with(599).validate(),
        Err(ConfigError::DurationBelowFloor {
            field: "claim_grace_s",
            duration: Duration::from_secs(599),
            floor: MIN_CLAIM_GRACE,
        })
    );
    assert_eq!(
        with(86_401).validate(),
        Err(ConfigError::DurationAboveCap {
            field: "claim_grace_s",
            duration: Duration::from_secs(86_401),
            cap: MAX_SHORT_DURATION,
        })
    );
    with(600).validate().expect("the floor itself is allowed");
    with(86_400)
        .validate()
        .expect("the ceiling itself is allowed");
    assert_eq!(
        with(599).validate().unwrap_err().to_string(),
        "claim_grace_s is 599s, below the floor of 600s"
    );
}

/// A claim may be asked until the quote's expiry, the permit window after it, and the
/// grace after that: the one window the claim, the pending store's eviction and a full
/// store's registration all read, so none of them lets a quote go before the others do.
#[test]
fn the_claim_window_is_the_permit_window_and_the_grace_after_it() {
    assert_eq!(
        Config::default().claim_window(),
        Duration::from_secs(120 + 3_600)
    );
    let short = Config {
        permit_deadline: Duration::from_secs(30),
        claim_grace: ClaimGrace::new(Duration::from_secs(600)),
        ..Config::default()
    };
    assert_eq!(short.claim_window(), Duration::from_secs(630));
}

/// The chains' CCTP minimum fees are a table like the other rail knobs: nothing of their
/// own while it is empty, so the golden lines do not move, a round trip when set, and a
/// minimum at or above the whole amount, which Circle's own setter refuses, is refused by
/// chain.
#[test]
fn the_cctp_minimum_fees_are_a_table_absent_while_empty_and_below_the_whole() {
    assert!(Config::default().cctp_min_fees.0.is_empty());
    assert_eq!(Config::default().to_bytes()[0], 0x91, "nothing of its own");
    let set = Config {
        cctp_min_fees: ChainTable(BTreeMap::from([(ChainId::BASE, CctpMinFee::new(1_000))])),
        ..Config::default()
    };
    assert_eq!(Config::from_bytes(set.to_bytes()), set);
    assert_eq!(
        set.to_bytes()[..2],
        [0x98, 29],
        "twenty-nine fields up to the table"
    );
    set.validate()
        .expect("a basis point is a minimum Circle may set");
    let whole = Config {
        cctp_min_fees: ChainTable(BTreeMap::from([(
            ChainId::ARBITRUM,
            CctpMinFee::new(MIN_FEE_MULTIPLIER),
        )])),
        ..Config::default()
    };
    assert_eq!(
        whole.validate(),
        Err(ConfigError::CctpMinFeeTooHigh {
            chain: ChainId::ARBITRUM,
            min_fee: CctpMinFee::new(MIN_FEE_MULTIPLIER),
            ceiling: MIN_FEE_MULTIPLIER,
        })
    );
}
