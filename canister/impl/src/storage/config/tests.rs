use super::*;
use std::collections::BTreeMap;
use types::ChainId;

const SECRET: &str = "https://eth-mainnet.g.alchemy.com/v2/secret-key";

/// The config change line's JSON is the redacted wire view, pinned to the byte: wire field
/// names in declaration order, maps in ascending chain id order, and every rpc url as
/// `***`.
#[test]
fn a_config_change_logs_json_of_the_redacted_wire_view() {
    let config = Config {
        rpc_urls: BTreeMap::from([(ChainId::ETHEREUM, SECRET.parse().unwrap())]),
        ecdsa_key_name: "key_1".to_string(),
        ..Config::default()
    };
    let json = change_json(&config);
    assert_eq!(
        json,
        concat!(
            r#"{"batch_window_ms":2000,"max_batch_items":10,"backstop_interval_s":30,"#,
            r#""simulate_before_sign":false,"#,
            r#""confirmations":{"1":1,"56":1,"137":6,"8453":1,"42161":1},"#,
            r#""rpc_urls":{"1":"***"},"ecdsa_key_name":"key_1","cctp_domains":{},"#,
            r#""eco_portal":null,"eco_enabled":false,"cctp_min_fees":{},"#,
            r#""books_check_interval_s":3600,"books_tolerance":"1000000","evm_chains":{},"#,
            r#""pool_chain":null,"credit_confirmations":{},"finalized_chains":null,"solana":null}"#
        )
    );
    assert!(!json.contains("secret-key"), "leaked: {json}");
}

/// The key name decides which threshold key every signature is made under, and the address
/// every vault on every chain is configured to obey is derived from that key. Changing it
/// after the derivation would sign under one key while the cache, the `evm_address` query
/// and the vaults all still name the other: every signature would fail the parity trial,
/// and each attempt would spend a nonce getting there. So the name is fixed once the
/// address exists.
#[test]
fn the_key_name_is_fixed_once_the_address_is_derived() {
    crate::storage::on_fresh_memory(|| {
        crate::storage::init();
        let key_1 = Config {
            ecdsa_key_name: "key_1".to_string(),
            ..Config::default()
        };
        let other = Config {
            ecdsa_key_name: "dfx_test_key".to_string(),
            ..Config::default()
        };
        test_set(key_1.clone());

        // before the derivation the name is a deploy-time choice like any other
        assert_eq!(ensure_key_name_is_not_moving(&other), Ok(()));

        crate::storage::ecdsa_address::set(types::EvmAddress::new([7; 20]));
        assert_eq!(
            ensure_key_name_is_not_moving(&other),
            Err(SetConfigError::Invalid(ConfigError::EcdsaKeyNameFixed {
                current: "key_1".to_string(),
                requested: "dfx_test_key".to_string(),
            })),
            "the address was derived under key_1 and cannot be re-derived under another"
        );

        // every other knob still moves, because the rule is about the name alone
        let same_name = Config {
            max_batch_items: 25,
            ..key_1
        };
        assert_eq!(ensure_key_name_is_not_moving(&same_name), Ok(()));
    });
}

#[test]
fn the_solana_line_holds_still_once_its_keys_are_derived() {
    crate::storage::on_fresh_memory(|| {
        crate::storage::init();
        let line = crate::solana::config_for_tests();
        let with = |solana| Config {
            solana,
            ..Config::default()
        };
        let other_name = types::solana::SolanaConfig {
            key_name: "dfx_test_key".to_string(),
            ..line.clone()
        };
        let other_program = types::solana::SolanaConfig {
            program_id: [8; 32],
            ..line.clone()
        };
        test_set(with(Some(line.clone())));

        // before the keys, all of it is a deploy-time choice
        assert_eq!(
            ensure_solana_line_is_not_moving(&with(Some(other_name.clone()))),
            Ok(())
        );
        assert_eq!(ensure_solana_line_is_not_moving(&with(None)), Ok(()));

        crate::storage::solana_keys::set([2; 32], [3; 32]);
        for refused in [
            with(Some(other_name)),
            with(None),
            with(Some(other_program.clone())),
        ] {
            assert!(matches!(
                ensure_solana_line_is_not_moving(&refused),
                Err(SetConfigError::Invalid(ConfigError::Solana { .. }))
            ));
        }
        // a switch still moves, and everything else
        let off = types::solana::SolanaConfig {
            enabled_gasless: false,
            enabled_arrivals: false,
            ..line.clone()
        };
        assert_eq!(
            ensure_solana_line_is_not_moving(&with(Some(off.clone()))),
            Ok(())
        );

        // with both switches off, between the keys and the deploy, the program id may still change
        test_set(with(Some(off)));
        assert_eq!(
            ensure_solana_line_is_not_moving(&with(Some(other_program))),
            Ok(())
        );
    });
}

/// The pool's gas rows (P11b) leave a config's ConfigChanged JSON as it was while they are
/// absent, and show in it once set.
#[test]
fn the_pools_gas_rows_show_in_the_json_only_once_set() {
    use types::config::{ChainTable, EvmChain, PerAction};
    let chain = |rows: bool| {
        let gas = PerAction::all(types::GasAmount::from(400_000_u64));
        let budget = PerAction::all(types::Wei::from(1_000_000_000_000_000_u64));
        let fees = PerAction::all(types::TokenAmount::from(50_000_u64));
        let strip = |mut gas: PerAction<types::GasAmount>| {
            if !rows {
                gas.pool_payout = None;
                gas.pool_transfer = None;
                gas.pool_approve = None;
            }
            gas
        };
        EvmChain {
            swapic: types::EvmAddress::new([1; 20]),
            runner: types::EvmAddress::new([2; 20]),
            cctp_receiver: types::EvmAddress::new([3; 20]),
            usdc: types::EvmAddress::new([4; 20]),
            token_messenger: types::EvmAddress::new([5; 20]),
            message_transmitter: types::EvmAddress::new([6; 20]),
            gas_limit: strip(gas),
            gas_budget: PerAction {
                pool_payout: rows.then_some(budget.relay_cctp),
                pool_transfer: rows.then_some(budget.relay_cctp),
                pool_approve: rows.then_some(budget.relay_cctp),
                pool_move: rows.then_some(budget.relay_cctp),
                pool_move_finish: rows.then_some(budget.relay_cctp),
                pool_ck_deposit: rows.then_some(budget.relay_cctp),
                eco_rescue: rows.then_some(budget.relay_cctp),
                ..budget
            },
            min_network_fee: PerAction {
                pool_payout: rows.then_some(fees.relay_cctp),
                pool_transfer: rows.then_some(fees.relay_cctp),
                pool_approve: rows.then_some(fees.relay_cctp),
                pool_move: rows.then_some(fees.relay_cctp),
                pool_move_finish: rows.then_some(fees.relay_cctp),
                pool_ck_deposit: rows.then_some(fees.relay_cctp),
                eco_rescue: rows.then_some(fees.relay_cctp),
                ..fees
            },
            usdc_name: None,
            usdc_version: None,
            block_time: std::time::Duration::from_secs(2),
        }
    };
    let config = |rows: bool| Config {
        evm_chains: ChainTable(BTreeMap::from([(ChainId::BASE, chain(rows))])),
        ..Config::default()
    };
    let without = change_json(&config(false));
    assert!(!without.contains("pool_payout"), "{without}");
    assert!(!without.contains("pool_approve"), "{without}");
    let with = change_json(&config(true));
    assert!(with.contains("\"pool_payout\":400000"), "{with}");
    assert!(with.contains("\"pool_transfer\":\"50000\""), "{with}");
}
