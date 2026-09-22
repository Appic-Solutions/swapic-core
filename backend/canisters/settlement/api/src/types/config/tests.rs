use super::*;
use crate::types::events::EvmAddressError;

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

/// The per-tick caps cross the wire as plain counts, and a cap outside its range comes back
/// named, so an operator sees which knob was refused.
#[test]
fn the_sweep_caps_cross_the_wire_and_a_bad_one_names_its_knob() {
    let wire = Config {
        max_refunds_per_sweep: 7,
        max_evictions_per_sweep: 9,
        ..Config::default()
    };
    let domain = types::Config::try_from(wire.clone()).unwrap();
    assert_eq!(domain.max_refunds_per_sweep.get(), 7);
    assert_eq!(domain.max_evictions_per_sweep.get(), 9);
    assert_eq!(Config::unredacted(domain), wire);

    let zero = types::Config::try_from(Config {
        max_refunds_per_sweep: 0,
        ..Config::default()
    })
    .unwrap();
    assert_eq!(
        ConfigError::from(zero.validate().unwrap_err()),
        ConfigError::CapOutOfRange {
            field: "max_refunds_per_sweep".to_string(),
            cap: 0,
            ceiling: 500
        }
    );
}

/// A duration knob above its cap is refused by name too, with the cap it broke, and in
/// milliseconds, so the one knob set in milliseconds is reported as it was set.
#[test]
fn a_duration_above_its_cap_names_its_knob_on_the_wire() {
    let domain = types::Config::try_from(Config {
        permit_deadline_s: 86_401,
        ..Config::default()
    })
    .unwrap();
    assert_eq!(
        ConfigError::from(domain.validate().unwrap_err()),
        ConfigError::DurationAboveCap {
            field: "permit_deadline_s".to_string(),
            duration_ms: 86_401_000,
            cap_ms: 86_400_000
        }
    );
    let domain = types::Config::try_from(Config {
        batch_window_ms: 60_500,
        ..Config::default()
    })
    .unwrap();
    assert_eq!(
        ConfigError::from(domain.validate().unwrap_err()),
        ConfigError::DurationAboveCap {
            field: "batch_window_ms".to_string(),
            duration_ms: 60_500,
            cap_ms: 60_000
        }
    );
}

/// The batch size is a cap like the per-pass ones, and a bad one names its knob.
#[test]
fn a_batch_size_outside_its_range_names_its_knob_on_the_wire() {
    let domain = types::Config::try_from(Config {
        max_batch_items: 0,
        ..Config::default()
    })
    .unwrap();
    assert_eq!(
        ConfigError::from(domain.validate().unwrap_err()),
        ConfigError::CapOutOfRange {
            field: "max_batch_items".to_string(),
            cap: 0,
            ceiling: 1_000
        }
    );
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

/// The deposit lookback crosses the wire as a plain block count like the per-pass caps,
/// and one outside its range comes back named.
#[test]
fn the_deposit_lookback_crosses_the_wire_and_a_bad_one_names_its_knob() {
    let wire = Config {
        deposit_lookback_blocks: 4_321,
        ..Config::default()
    };
    let domain = types::Config::try_from(wire.clone()).unwrap();
    assert_eq!(domain.deposit_lookback_blocks.get(), 4_321);
    assert_eq!(Config::unredacted(domain), wire);
    assert_eq!(Config::default().deposit_lookback_blocks, 10_000);

    let zero = types::Config::try_from(Config {
        deposit_lookback_blocks: 0,
        ..Config::default()
    })
    .unwrap();
    assert_eq!(
        ConfigError::from(zero.validate().unwrap_err()),
        ConfigError::CapOutOfRange {
            field: "deposit_lookback_blocks".to_string(),
            cap: 0,
            ceiling: 1_000_000
        }
    );
}

/// The rail knobs cross the wire as chain-keyed tables and optional addresses, print as
/// EIP-55 text, and a value that is not an address is refused by knob and chain.
#[test]
fn the_rail_knobs_cross_the_wire_and_a_bad_address_names_its_knob() {
    let wire = Config {
        cctp_domains: BTreeMap::from([(8453, 6), (42161, 3)]),
        usdc_addresses: BTreeMap::from([(
            8453,
            "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".to_string(),
        )]),
        token_messenger: Some("0x28b5a0e9C621a5BadaA536219b3a228C8168cf5d".to_string()),
        message_transmitter: Some("0x81D40F21F12A8F0E3252Bccb954D722d4c464B64".to_string()),
        eco_portal: Some("0xEC000064576f9C95a8623Bc0eff3db6d296ea6df".to_string()),
        ..Config::default()
    };
    let domain = types::Config::try_from(wire.clone()).unwrap();
    assert_eq!(
        domain.cctp_domains.get(types::ChainId::BASE),
        Some(types::rail::CctpDomain::new(6))
    );
    assert_eq!(Config::unredacted(domain), wire);

    // a lower-case spelling comes back checksummed: the address is the bytes
    let lower = Config {
        eco_portal: Some("0xec000064576f9c95a8623bc0eff3db6d296ea6df".to_string()),
        ..Config::default()
    };
    let domain = types::Config::try_from(lower).unwrap();
    assert_eq!(
        Config::unredacted(domain).eco_portal,
        Some("0xEC000064576f9C95a8623Bc0eff3db6d296ea6df".to_string())
    );

    let bad_usdc = Config {
        usdc_addresses: BTreeMap::from([(8453, "usdc".to_string())]),
        ..Config::default()
    };
    assert_eq!(
        types::Config::try_from(bad_usdc).map(drop),
        Err(types::ConfigError::NotAnAddress {
            field: "usdc_addresses",
            chain: Some(types::ChainId::BASE),
            reason: types::evm::EvmAddressError::NoPrefix,
        })
    );
    let bad_portal = Config {
        eco_portal: Some("0x12".to_string()),
        ..Config::default()
    };
    let refused = types::Config::try_from(bad_portal).unwrap_err();
    assert_eq!(
        ConfigError::from(refused),
        ConfigError::NotAnAddress {
            field: "eco_portal".to_string(),
            chain_id: None,
            reason: EvmAddressError::WrongLength { len: 2 },
        }
    );
}
