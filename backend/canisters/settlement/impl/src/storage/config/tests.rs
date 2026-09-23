use super::*;
use std::collections::BTreeMap;
use types::{BasisPoints, ChainId};

const SECRET: &str = "https://eth-mainnet.g.alchemy.com/v2/secret-key";

/// The line is hashed into the chain at its first append, so its shape is pinned here to
/// the byte: wire field names in declaration order, maps in ascending chain id order,
/// `max_swap_usd` as a decimal string, and every rpc url as `***`.
///
/// Rewritten for fix wave 5 (N7): the wire view gained `claim_grace_s`, so the line ends
/// with it.
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
            r#""confirmations":{"1":12,"56":1,"137":6,"8453":1,"42161":1},"#,
            r#""rpc_urls":{"1":"***"},"vault_addresses":{"8453":"0xvault"},"#,
            r#""ecdsa_key_name":"key_1","max_refunds_per_sweep":50,"#,
            r#""max_evictions_per_sweep":200,"audit_chunk_events":1000,"#,
            r#""deposit_lookback_blocks":345600,"cctp_domains":{},"usdc_addresses":{},"#,
            r#""token_messenger":null,"message_transmitter":null,"eco_portal":null,"#,
            r#""eco_enabled":false,"claim_grace_s":3600}"#
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
