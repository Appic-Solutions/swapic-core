use super::*;

extern crate alloc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use serde_json::Value;

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn routes() -> Vec<Value> {
    let text = include_str!("../../vectors/jupiter_routes.json");
    serde_json::from_str::<Value>(text)
        .unwrap()
        .as_array()
        .unwrap()
        .clone()
}

fn text(v: &Value, k: &str) -> String {
    String::from(v[k].as_str().unwrap())
}

#[test]
fn every_captured_route_decodes_to_its_quoted_amounts() {
    let all = routes();
    assert!(all.len() >= 150, "the captures hold {} routes", all.len());
    let (mut route, mut shared) = (0, 0);
    for v in &all {
        let data = hex(v["data"].as_str().unwrap());
        assert!(is_allowed_route(&data));
        let parsed = Route::parse(&data).unwrap();
        assert_eq!(parsed.in_amount.to_string(), text(v, "in_amount"));
        assert_eq!(parsed.quoted_out.to_string(), text(v, "out_amount"));
        assert_eq!(
            parsed.slippage_bps as i64,
            v["slippage_bps"].as_i64().unwrap()
        );
        assert_eq!(parsed.platform_fee_bps, 0);
        match v["kind"].as_str().unwrap() {
            "shared" => {
                assert_eq!(parsed.kind, RouteKind::SharedAccountsRouteV2);
                shared += 1;
            }
            _ => {
                assert_eq!(parsed.kind, RouteKind::RouteV2);
                route += 1;
            }
        }
        // the threshold Jupiter answered is what the arguments imply
        let threshold: u64 = text(v, "threshold").parse().unwrap();
        assert!(
            parsed.minimum_out().abs_diff(threshold) <= 1,
            "{} vs {}",
            parsed.minimum_out(),
            threshold
        );
    }
    assert!(route > 0 && shared > 0);
}

#[test]
fn the_fixed_account_positions_hold_the_route_mints() {
    for v in routes() {
        assert_eq!(text(&v, "source_mint"), text(&v, "input_mint"));
        assert_eq!(text(&v, "destination_mint"), text(&v, "output_mint"));
    }
}

#[test]
fn anything_but_the_two_routes_is_refused() {
    // v1 `route`, `shared_accounts_route`, `set_token_ledger`-like data and short data
    assert!(!is_allowed_route(&[
        229, 23, 203, 151, 122, 227, 173, 42, 0, 0
    ]));
    assert!(!is_allowed_route(&[193, 32, 155, 51, 65, 214, 156, 129]));
    assert!(!is_allowed_route(&ROUTE_V2[..7]));
    assert_eq!(
        Route::parse(&[1, 2, 3]),
        Err(SwapicError::JupiterInstructionNotAllowed)
    );
    assert_eq!(
        Route::parse(&ROUTE_V2),
        Err(SwapicError::JupiterInstructionNotAllowed)
    );
}
