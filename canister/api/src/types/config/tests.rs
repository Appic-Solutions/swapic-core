use super::*;
use crate::types::events::EvmAddressError;

const SECRET: &str = "https://eth-mainnet.g.alchemy.com/v2/secret-key";

/// What a real deploy sends: two provider urls, both of them secrets.
fn secret_bearing() -> Config {
    Config {
        rpc_urls: BTreeMap::from([
            (1, SECRET.to_string()),
            (8453, "https://base.example/rpc?key=hunter2".to_string()),
        ]),
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
    assert_eq!(
        Config {
            rpc_urls: full.rpc_urls.clone(),
            ..public
        },
        full
    );
}

/// The full config crosses the wire in `get_config_full` and `set_config`, so a stray debug
/// print of either must not show a key.
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
    assert!(shown.contains("key_1"));
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
fn the_redacted_placeholder_is_no_rpc_url() {
    let public = Config::from(types::Config::try_from(secret_bearing()).unwrap());
    let err = types::Config::try_from(public).unwrap_err();
    assert_eq!(
        err,
        types::ConfigError::RedactedRpcUrl {
            chain: ChainId::ETHEREUM
        }
    );
    assert_eq!(
        ConfigError::from(err),
        ConfigError::RedactedRpcUrl { chain_id: 1 }
    );
}

/// Every duration crosses the wire in its own unit.
#[test]
fn every_duration_keeps_its_unit() {
    let wire = Config {
        batch_window_ms: 1_500,
        backstop_interval_s: 20,
        books_check_interval_s: 1_800,
        ..Config::default()
    };
    let domain = types::Config::try_from(wire.clone()).unwrap();
    assert_eq!(domain.batch_window, Duration::from_millis(1_500));
    assert_eq!(domain.backstop_interval, Duration::from_secs(20));
    assert_eq!(domain.books_check_interval.0, Duration::from_secs(1_800));
    assert_eq!(Config::unredacted(domain), wire);
    assert_eq!(Config::default().books_check_interval_s, 3_600);
}

/// A duration knob above its cap is refused by name, with the cap it broke, in
/// milliseconds.
#[test]
fn a_duration_above_its_cap_names_its_knob_on_the_wire() {
    let domain = types::Config::try_from(Config {
        backstop_interval_s: 61,
        ..Config::default()
    })
    .unwrap();
    assert_eq!(
        ConfigError::from(domain.validate().unwrap_err()),
        ConfigError::DurationAboveCap {
            field: "backstop_interval_s".to_string(),
            duration_ms: 61_000,
            cap_ms: 60_000
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

/// The batch size is a cap, and a bad one names its knob.
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

/// Arbitrum as design C runs on it, USDC's domain left as `name` and `version` say.
fn arbitrum(name: Option<&str>, version: Option<&str>) -> EvmChainConfig {
    EvmChainConfig {
        swapic: "0xF2E9d13Ba690c71cfF7788DE7273Ca7658d6D7C7".to_string(),
        runner: "0xd9a6d4123BcF737A36e5f66D474891f15E8d2d3C".to_string(),
        cctp_receiver: "0x5555555555555555555555555555555555555555".to_string(),
        usdc: "0xaf88d065e77c8cC2239327C5EDb3A432268e5831".to_string(),
        token_messenger: "0x28b5a0e9C621a5BadaA536219b3a228C8168cf5d".to_string(),
        message_transmitter: "0x81D40F21F12A8F0E3252Bccb954D722d4c464B64".to_string(),
        gas_limit: ActionGas {
            relay_cctp: 400_000,
            complete_swap: 1_200_000,
            sweep: Some(80_000),
            ..ActionGas::default()
        },
        gas_budget_wei: ActionAmounts {
            relay_cctp: Nat::from(1_000_000_000_000_000_u64),
            ..ActionAmounts::default()
        },
        min_network_fee: ActionAmounts {
            complete_swap: Nat::from(60_000_u64),
            ..ActionAmounts::default()
        },
        usdc_domain_name: name.map(str::to_string),
        usdc_domain_version: version.map(str::to_string),
        block_time_ms: 250,
    }
}

/// The rail knobs cross the wire as chain-keyed tables and optional addresses, print as
/// EIP-55 text, and a value that is not an address is refused by knob and chain.
#[test]
fn the_rail_knobs_cross_the_wire_and_a_bad_address_names_its_knob() {
    let wire = Config {
        cctp_domains: BTreeMap::from([(8453, 6), (42161, 3)]),
        evm_chains: BTreeMap::from([(42161, arbitrum(None, None))]),
        eco_portal: Some("0xEC000064576f9C95a8623Bc0eff3db6d296ea6df".to_string()),
        ..Config::default()
    };
    let domain = types::Config::try_from(wire.clone()).unwrap();
    let chain = domain.evm_chains.get(types::ChainId::ARBITRUM).unwrap();
    assert_eq!(
        chain.gas_limit.complete_swap,
        types::GasAmount::from(1_200_000_u64)
    );
    assert_eq!(
        chain.gas_limit.get(types::config::GasAction::Sweep),
        Some(types::GasAmount::from(80_000_u64))
    );
    assert_eq!(chain.block_time, Duration::from_millis(250));
    assert_eq!(Config::unredacted(domain), wire);

    let lower = Config {
        eco_portal: Some("0xec000064576f9c95a8623bc0eff3db6d296ea6df".to_string()),
        ..Config::default()
    };
    let domain = types::Config::try_from(lower).unwrap();
    assert_eq!(
        Config::unredacted(domain).eco_portal,
        Some("0xEC000064576f9C95a8623Bc0eff3db6d296ea6df".to_string())
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

/// A chain's block time outside 100 ms to a minute is refused by chain, in milliseconds.
#[test]
fn a_block_time_out_of_range_names_its_chain_on_the_wire() {
    let domain = types::Config::try_from(Config {
        evm_chains: BTreeMap::from([(
            42161,
            EvmChainConfig {
                block_time_ms: 50,
                ..arbitrum(None, None)
            },
        )]),
        ..Config::default()
    })
    .unwrap();
    assert_eq!(
        ConfigError::from(domain.validate().unwrap_err()),
        ConfigError::BlockTimeOutOfRange {
            chain_id: 42161,
            block_time_ms: 50
        }
    );
}

/// A chain's USDC domain name and version cross the wire as set, none staying none, and a
/// text that is no domain name or version is refused by its knob.
#[test]
fn a_chains_usdc_domain_crosses_the_wire_and_a_bad_one_names_its_knob() {
    let wire = |chain: EvmChainConfig| Config {
        evm_chains: BTreeMap::from([(42161, chain)]),
        ..Config::default()
    };
    let named = wire(arbitrum(Some("USDC"), None));
    let domain = types::Config::try_from(named.clone()).unwrap();
    let chain = domain.evm_chains.get(types::ChainId::ARBITRUM).unwrap();
    let usdc = chain.usdc_domain();
    assert_eq!((usdc.name, usdc.version), ("USDC", "2"));
    assert_eq!(Config::unredacted(domain), named);
    let empty = types::Config::try_from(wire(arbitrum(Some(""), None))).unwrap_err();
    assert_eq!(
        ConfigError::from(empty),
        ConfigError::NotDomainText {
            field: "evm_chains.usdc_domain_name".to_string(),
            chain_id: 42161,
        }
    );
}

/// The chains' CCTP minimum fees cross the wire as a chain-keyed table of Circle's own
/// units, and one at or above the whole amount is refused by chain.
#[test]
fn the_cctp_minimum_fees_cross_the_wire_and_a_bad_one_names_its_chain() {
    let wire = Config {
        cctp_min_fees: BTreeMap::from([(8453, 1_000), (42161, 0)]),
        ..Config::default()
    };
    let domain = types::Config::try_from(wire.clone()).unwrap();
    assert_eq!(Config::unredacted(domain), wire);
    let whole = types::Config::try_from(Config {
        cctp_min_fees: BTreeMap::from([(8453, 10_000_000)]),
        ..Config::default()
    })
    .unwrap();
    assert_eq!(
        ConfigError::from(whole.validate().unwrap_err()),
        ConfigError::CctpMinFeeTooHigh {
            chain_id: 8453,
            min_fee: 10_000_000,
            ceiling: 10_000_000
        }
    );
}

/// The optional gas rows, the sweep's included: absent, the domain rows are absent; set,
/// they cross the wire and back.
#[test]
fn the_optional_gas_rows_cross_the_wire() {
    use candid::{Decode, Encode};
    let absent = ActionGas::default();
    let domain: types::config::PerAction<types::GasAmount> = absent.into();
    assert_eq!(domain.get(types::config::GasAction::PoolPayout), None);
    assert_eq!(domain.get(types::config::GasAction::Sweep), None);
    let set = ActionGas {
        pool_payout: Some(500_000),
        pool_transfer: Some(80_000),
        pool_approve: Some(60_000),
        sweep: Some(70_000),
        ..ActionGas::default()
    };
    let domain: types::config::PerAction<types::GasAmount> = set.into();
    assert_eq!(
        domain.get(types::config::GasAction::Sweep),
        Some(types::GasAmount::from(70_000_u64))
    );
    assert_eq!(ActionGas::from(domain), set);
    let bytes = Encode!(&set).unwrap();
    assert_eq!(Decode!(&bytes, ActionGas).unwrap(), set);
    let amounts = ActionAmounts {
        sweep: Some(candid::Nat::from(10_000_u32)),
        ..ActionAmounts::default()
    };
    let bytes = Encode!(&amounts).unwrap();
    assert_eq!(Decode!(&bytes, ActionAmounts).unwrap(), amounts);
}
