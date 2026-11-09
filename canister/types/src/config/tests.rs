use super::*;
use crate::evm::EvmAddress;
use ic_stable_structures::Storable;

/// The defaults are spec numbers, not preferences: this test is the spec.
#[test]
fn defaults_match_spec() {
    let c = Config::default();
    assert_eq!(c.batch_window, Duration::from_millis(2_000));
    assert_eq!(c.max_batch_items, 10);
    assert_eq!(c.backstop_interval, Duration::from_secs(30));
    assert!(!c.simulate_before_sign);
    assert_eq!(
        c.confirmations,
        BTreeMap::from(
            [(1, 1), (8453, 1), (56, 1), (137, 6), (42161, 1)]
                .map(|(chain, depth)| (ChainId::new(chain), BlockDepth::new(depth)))
        )
    );
    assert_eq!(c.credit_confirmations, ChainTable::default());
    assert_eq!(
        c.books_check_interval,
        BooksInterval(Duration::from_secs(3_600))
    );
    assert!(!c.eco_enabled.is_on());
    // deploy-time values, empty in code
    assert!(c.rpc_urls.is_empty());
    assert!(c.evm_chains.0.is_empty());
    assert_eq!(c.ecdsa_key_name, "dfx_test_key");
}

const SECRET: &str = "https://eth-mainnet.g.alchemy.com/v2/secret-key";

/// What a real deploy looks like: two provider urls, both of them secrets.
fn secret_bearing() -> Config {
    Config {
        rpc_urls: BTreeMap::from([
            (ChainId::ETHEREUM, SECRET.parse().unwrap()),
            (
                ChainId::BASE,
                "https://base.example/rpc?key=hunter2".parse().unwrap(),
            ),
        ]),
        ecdsa_key_name: "key_1".to_string(),
        ..Config::default()
    }
}

/// A chain entry as a deploy writes one.
fn base_chain() -> EvmChain {
    EvmChain {
        swapic: EvmAddress::new([0x5a; 20]),
        runner: EvmAddress::new([0x5b; 20]),
        cctp_receiver: EvmAddress::new([0x5c; 20]),
        usdc: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"
            .parse()
            .unwrap(),
        token_messenger: "0x28b5a0e9C621a5BadaA536219b3a228C8168cf5d"
            .parse()
            .unwrap(),
        message_transmitter: EvmAddress::new([0x81; 20]),
        gas_limit: PerAction::all(GasAmount::from(400_000_u64)),
        gas_budget: PerAction::all(Wei::from(1_000_000_000_000_000_u64)),
        min_network_fee: PerAction::all(TokenAmount::from(50_000_u64)),
        usdc_name: None,
        usdc_version: None,
        block_time: Duration::from_secs(2),
        usdc_decimals: None,
        across: None,
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
    assert!(shown.contains("key_1"));
}

#[test]
fn defaults_are_valid() {
    Config::default()
        .validate()
        .expect("the defaults are valid");
}

#[test]
fn validate_rejects_an_empty_ecdsa_key_name() {
    let config = Config {
        ecdsa_key_name: String::new(),
        ..Config::default()
    };
    assert_eq!(config.validate(), Err(ConfigError::EmptyEcdsaKeyName));
}

/// Crediting decides money arriving at the canister, and Ethereum mainnet reorgs
/// routinely: a config that would credit a deposit there at less than the floor is
/// refused, whatever else it holds. Every other chain is the deploy's own call.
#[test]
fn validate_rejects_a_mainnet_crediting_depth_below_the_floor() {
    for depth in [1, MIN_ETHEREUM_CONFIRMATIONS.get() - 1] {
        let shallow = Config {
            credit_confirmations: ChainTable(BTreeMap::from([(
                ChainId::ETHEREUM,
                BlockDepth::new(depth),
            )])),
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
        credit_confirmations: ChainTable(BTreeMap::from([(
            ChainId::ETHEREUM,
            MIN_ETHEREUM_CONFIRMATIONS,
        )])),
        ..Config::default()
    };
    assert_eq!(at_the_floor.validate(), Ok(()));
    let shallow_elsewhere = Config {
        credit_confirmations: ChainTable(BTreeMap::from([(ChainId::ARBITRUM, BlockDepth::new(1))])),
        ..Config::default()
    };
    assert_eq!(shallow_elsewhere.validate(), Ok(()));
}

/// Only a moved check interval restarts the hourly pass; the backstop, a one-shot, restarts
/// nothing.
#[test]
fn interval_changes_flag_the_hourly_pass_for_its_own_interval_only() {
    let base = Config::default();
    let changes = |new: Config| base.interval_changes(&new).books;
    let unrelated = Config {
        max_batch_items: 20,
        ..Config::default()
    };
    assert!(!changes(unrelated));
    let backstop = Config {
        backstop_interval: Duration::from_secs(31),
        ..Config::default()
    };
    assert!(!changes(backstop));
    let books = Config {
        books_check_interval: BooksInterval(Duration::from_secs(1_800)),
        ..Config::default()
    };
    assert!(changes(books));
}

/// The backstop drives on the interval the config names, at most every minute, and on the
/// floor every timer is held to when the knob is below it.
#[test]
fn the_backstop_drives_on_the_configured_interval() {
    let with = |backstop_interval| {
        Config {
            backstop_interval,
            ..Config::default()
        }
        .engine_every()
    };
    assert_eq!(Config::default().engine_every(), Duration::from_secs(30));
    assert_eq!(with(Duration::from_secs(5)), Duration::from_secs(5));
    assert_eq!(with(Duration::from_secs(600)), Duration::from_secs(60));
    assert_eq!(with(Duration::ZERO), Duration::from_secs(1));
}

#[test]
fn validate_rejects_a_check_interval_above_a_year() {
    let over = MAX_TIMER_INTERVAL + Duration::from_secs(1);
    assert_eq!(
        Config {
            books_check_interval: BooksInterval(over),
            ..Config::default()
        }
        .validate(),
        Err(ConfigError::TimerIntervalTooLong {
            field: "books_check_interval_s",
            interval: over
        })
    );
    Config {
        books_check_interval: BooksInterval(MAX_TIMER_INTERVAL),
        ..Config::default()
    }
    .validate()
    .expect("a year exactly is allowed");
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
    secret_bearing().validate().expect("real urls pass");
}

/// Each duration knob is bounded, and each rejection names its field and its cap.
#[test]
fn validate_rejects_every_duration_knob_above_its_cap() {
    type Knob = (&'static str, Duration, fn(Duration) -> Config);
    let knobs: [Knob; 2] = [
        ("backstop_interval_s", ENGINE_EVERY_MAX, |d| Config {
            backstop_interval: d,
            ..Config::default()
        }),
        ("batch_window_ms", MAX_BATCH_WINDOW, |d| Config {
            batch_window: d,
            ..Config::default()
        }),
    ];
    for (field, cap, with) in knobs {
        let over = cap + Duration::from_millis(500);
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
    }
}

/// A batch of nothing is no batch at all, and an unbounded one is the trap the caps exist
/// to prevent.
#[test]
fn validate_rejects_a_batch_size_outside_its_range() {
    let batch = |items| Config {
        max_batch_items: items,
        ..Config::default()
    };
    for items in [0, MAX_BATCH_ITEMS + 1] {
        assert_eq!(
            batch(items).validate(),
            Err(ConfigError::CapOutOfRange {
                field: "max_batch_items",
                cap: items,
                ceiling: MAX_BATCH_ITEMS
            })
        );
    }
    batch(MAX_BATCH_ITEMS)
        .validate()
        .expect("the ceiling itself is allowed");
    batch(1).validate().expect("one item a batch is allowed");
}

/// A chain's block time paces the outbox's receipt reads, so one outside 100 ms to a minute
/// is a typo, refused by chain.
#[test]
fn validate_holds_each_block_time_to_its_range() {
    let with = |block_time| Config {
        evm_chains: ChainTable(BTreeMap::from([(
            ChainId::BASE,
            EvmChain {
                block_time,
                ..base_chain()
            },
        )])),
        ..Config::default()
    };
    for block_time in [Duration::from_millis(99), Duration::from_millis(60_001)] {
        assert_eq!(
            with(block_time).validate(),
            Err(ConfigError::BlockTimeOutOfRange {
                chain: ChainId::BASE,
                block_time,
            })
        );
    }
    with(MIN_BLOCK_TIME)
        .validate()
        .expect("the floor is allowed");
    with(MAX_BLOCK_TIME)
        .validate()
        .expect("the ceiling is allowed");
}

/// The stored copy is the operative one, so it keeps the real urls and every chain entry.
#[test]
fn storage_keeps_the_secrets_and_the_chains() {
    let config = Config {
        evm_chains: ChainTable(BTreeMap::from([(ChainId::BASE, base_chain())])),
        cctp_domains: ChainTable(BTreeMap::from([(ChainId::BASE, CctpDomain::new(6))])),
        eco_enabled: EcoEnabled::ON,
        simulate_before_sign: true,
        ..secret_bearing()
    };
    let back = Config::from_bytes(config.to_bytes());
    assert_eq!(back, config);
    assert_eq!(back.rpc_urls[&ChainId::ETHEREUM].expose(), SECRET);
}

/// A duration knob is refused with the value it was set to, not a rounding of it.
#[test]
fn a_duration_error_keeps_its_milliseconds() {
    let err = Config {
        batch_window: Duration::from_millis(60_500),
        ..Config::default()
    }
    .validate()
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "batch_window_ms is 60.5s, above the cap of 60s"
    );
}

/// A minimum at or above the whole amount, which Circle's own setter refuses, is refused by
/// chain.
#[test]
fn a_cctp_minimum_fee_stays_below_the_whole() {
    let set = Config {
        cctp_min_fees: ChainTable(BTreeMap::from([(ChainId::BASE, CctpMinFee::new(1_000))])),
        ..Config::default()
    };
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

/// Every action has its own row, the sweep's included, and `all` sets every row.
#[test]
fn every_gas_action_has_its_row_and_its_name() {
    let rows = PerAction::all(7_u64);
    for action in GasAction::ALL {
        assert_eq!(rows.get(action), Some(7));
        assert_eq!(GasAction::from_name(action.name()), Some(action));
    }
    assert_eq!(GasAction::Sweep.name(), "sweep");
    assert_eq!(PerAction::<u64>::default().get(GasAction::Sweep), None);
}

/// Only Polygon may wait for its finalized block: any other chain is refused by name.
#[test]
fn only_polygon_may_wait_for_its_finalized_block() {
    let with = |chains: &[ChainId]| Config {
        finalized: ChainSet(chains.iter().copied().collect()),
        ..Config::default()
    };
    assert_eq!(with(&[]).validate(), Ok(()));
    assert_eq!(with(&[ChainId::POLYGON]).validate(), Ok(()));
    assert_eq!(
        with(&[ChainId::POLYGON, ChainId::BASE]).validate(),
        Err(ConfigError::NotFinalizable {
            chain: ChainId::BASE
        })
    );
}
