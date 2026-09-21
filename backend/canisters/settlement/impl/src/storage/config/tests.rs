use super::*;
use std::collections::BTreeMap;
use types::{BasisPoints, ChainId};

const SECRET: &str = "https://eth-mainnet.g.alchemy.com/v2/secret-key";

/// The line is hashed into the chain at its first append, so its shape is pinned here to
/// the byte: wire field names in declaration order, maps in ascending chain id order,
/// `max_swap_usd` as a decimal string, and every rpc url as `***`.
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
            r#""max_evictions_per_sweep":200,"audit_chunk_events":1000}"#
        )
    );
    assert!(!json.contains("secret-key"), "leaked: {json}");
}
