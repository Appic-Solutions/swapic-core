use super::*;
use std::collections::BTreeMap;
use types::{BasisPoints, ChainId};

const SECRET: &str = "https://eth-mainnet.g.alchemy.com/v2/secret-key";

/// The line is hashed into the chain at its first append, so its shape is pinned here to
/// the byte: wire field names in declaration order, maps in ascending chain id order,
/// `max_swap_usd` as a decimal string, and every rpc url as `***`.
///
/// Rewritten for fix wave 5 (N7, N12): the wire view gained `claim_grace_s` and then
/// `cctp_min_fees`, so the line ends with them.
/// Rewritten for fix wave 6 (M1): the default lookback is the ceiling, 400,000 blocks.
/// Rewritten for P10: the gas budgets, the minimum network fees and the books knobs.
/// Rewritten for P10c: Ethereum's swap depth is 1, and the wire view gained
/// `credit_confirmations`, so the line ends with it.
#[test]
fn a_config_change_logs_json_of_the_redacted_wire_view() {
    let config = Config {
        platform_fee: BasisPoints::new(10),
        rpc_urls: BTreeMap::from([(ChainId::ETHEREUM, SECRET.parse().unwrap())]),
        vault_addresses: BTreeMap::from([(ChainId::BASE, "0xvault".parse().unwrap())]),
        ecdsa_key_name: "key_1".to_string(),
        ..Config::default()
    };
    let json = change_json(&config);
    assert_eq!(
        json,
        concat!(
            r#"{"platform_fee_bps":10,"max_fee_bps":30,"max_swap_usd":"1000","quote_ttl_s":45,"#,
            r#""permit_deadline_s":120,"chain_data_max_age_s":10,"batch_window_ms":2000,"#,
            r#""max_batch_items":10,"decision_timeout_min":30,"rail_status_max_age_s":30,"#,
            r#""simulate_before_sign":false,"expiry_check_interval_s":60,"#,
            r#""replay_audit_interval_s":21600,"#,
            r#""confirmations":{"1":1,"56":1,"137":6,"8453":1,"42161":1},"#,
            r#""rpc_urls":{"1":"***"},"vault_addresses":{"8453":"0xvault"},"#,
            r#""ecdsa_key_name":"key_1","max_refunds_per_sweep":50,"#,
            r#""max_evictions_per_sweep":200,"audit_chunk_events":1000,"#,
            r#""deposit_lookback_blocks":400000,"cctp_domains":{},"usdc_addresses":{},"#,
            r#""eco_portal":null,"#,
            r#""eco_enabled":false,"claim_grace_s":3600,"cctp_min_fees":{},"#,
            r#""books_check_interval_s":600,"books_tolerance":"1000000","evm_chains":{},"#,
            r#""pool_chain":null,"credit_confirmations":{}}"#
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
