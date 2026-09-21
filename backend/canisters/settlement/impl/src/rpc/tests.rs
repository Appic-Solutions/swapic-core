use super::*;
use serde_json::json;

fn url() -> RpcUrl {
    "https://base-mainnet.example/v2/secret-key"
        .parse()
        .expect("a test url is not the redaction placeholder")
}

fn calls() -> Vec<(&'static str, Value)> {
    vec![
        ("eth_blockNumber", json!([])),
        ("eth_getBalance", json!(["0xabc", "latest"])),
    ]
}

/// The whole point of the outcall policy, pinned where it is built: one replica, and
/// version 2 pricing, without which the replication mode buys nothing at all (measured on
/// mainnet: 87.44M replicated against 87.56M unreplicated under version 1).
#[test]
fn every_outcall_is_unreplicated_and_priced_pay_as_you_go() {
    let request = post(&url(), b"{}".to_vec(), 2_048);
    assert_eq!(request.is_replicated, Some(false));
    assert_eq!(request.pricing_version, Some(PRICING_VERSION_PAY_AS_YOU_GO));
    assert_eq!(
        request.transform, None,
        "an unreplicated call reaches no consensus, so it needs no transform"
    );
    assert_eq!(request.max_response_bytes, Some(2_048));
    assert_eq!(request.method, HttpMethod::POST);
    assert_eq!(request.body.as_deref(), Some(&b"{}"[..]));
    assert_eq!(
        request.headers,
        vec![HttpHeader {
            name: "Content-Type".to_string(),
            value: "application/json".to_string(),
        }]
    );
    assert_eq!(request.url, url().expose());
}

/// A batch is one request: an array with one entry per call, numbered from zero in call
/// order, which is what lets the replies be sorted back into it.
#[test]
fn a_batch_body_is_one_array_with_an_id_per_call() {
    let body = batch_body(&calls());
    let sent: Value = serde_json::from_slice(&body).expect("the body is json");
    assert_eq!(
        sent,
        json!([
            {"jsonrpc": "2.0", "id": 0, "method": "eth_blockNumber", "params": []},
            {"jsonrpc": "2.0", "id": 1, "method": "eth_getBalance", "params": ["0xabc", "latest"]},
        ])
    );
}

/// A single call is a plain object, never an array of one: a provider that answers an
/// object to an array, or the other way round, is a provider this canister would misread.
#[test]
fn a_single_call_is_not_wrapped_in_an_array() {
    let body = single_body("eth_chainId", &json!([]));
    let sent: Value = serde_json::from_slice(&body).expect("the body is json");
    assert_eq!(
        sent,
        json!({"jsonrpc": "2.0", "id": 0, "method": "eth_chainId", "params": []})
    );
}

/// JSON-RPC lets a server answer a batch in any order, so the ids are the order and the
/// array is not.
#[test]
fn replies_answered_out_of_order_are_sorted_by_id() {
    let answered = json!([
        {"jsonrpc": "2.0", "id": 1, "result": "0x2"},
        {"jsonrpc": "2.0", "id": 0, "result": "0x1"},
    ]);
    assert_eq!(
        parse_batch(answered.to_string().as_bytes(), &["a", "b"]),
        Ok(vec![json!("0x1"), json!("0x2")])
    );
}

/// An error member is an answer, not a transport failure, and the refusal names the method
/// that produced it: a batch of five reads is otherwise unreadable.
#[test]
fn a_reply_carrying_an_error_names_the_method_that_answered_it() {
    let answered = json!([
        {"jsonrpc": "2.0", "id": 0, "result": "0x1"},
        {"jsonrpc": "2.0", "id": 1, "error": {"code": -32000, "message": "header not found"}},
    ]);
    assert_eq!(
        parse_batch(
            answered.to_string().as_bytes(),
            &["eth_blockNumber", "eth_getLogs"]
        ),
        Err(RpcError::Rpc {
            method: "eth_getLogs".to_string(),
            message: "header not found".to_string(),
        })
    );
}

/// Every way a batch reply can fail to be the batch that was asked for.
#[test]
fn a_reply_that_is_not_the_batch_asked_for_is_a_json_error() {
    let cases = [
        json!([{"jsonrpc": "2.0", "id": 0, "result": "0x1"}]),
        json!([
            {"jsonrpc": "2.0", "id": 0, "result": "0x1"},
            {"jsonrpc": "2.0", "id": 0, "result": "0x2"},
        ]),
        json!([
            {"jsonrpc": "2.0", "id": 0, "result": "0x1"},
            {"jsonrpc": "2.0", "id": 7, "result": "0x2"},
        ]),
        json!({"jsonrpc": "2.0", "id": 0, "result": "0x1"}),
        json!([
            {"jsonrpc": "2.0", "id": 0},
            {"jsonrpc": "2.0", "id": 1, "result": "0x2"},
        ]),
    ];
    for answered in cases {
        assert!(
            matches!(
                parse_batch(answered.to_string().as_bytes(), &["a", "b"]),
                Err(RpcError::Json(_))
            ),
            "{answered} was read as a batch of two"
        );
    }
    assert!(matches!(
        parse_batch(b"not json at all", &["a"]),
        Err(RpcError::Json(_))
    ));
}

/// A single reply is read by the same rules, and its error names its own method.
#[test]
fn a_single_reply_answers_its_result_or_names_its_method() {
    let ok = json!({"jsonrpc": "2.0", "id": 0, "result": "0x1"});
    assert_eq!(
        parse_single(ok.to_string().as_bytes(), "eth_chainId"),
        Ok(json!("0x1"))
    );
    let failed = json!({"jsonrpc": "2.0", "id": 0, "error": {"message": "nope"}});
    assert_eq!(
        parse_single(failed.to_string().as_bytes(), "eth_chainId"),
        Err(RpcError::Rpc {
            method: "eth_chainId".to_string(),
            message: "nope".to_string(),
        })
    );
}

/// The attachment is a reservation, not a payment: it has to cover the worst response the
/// cap admits on the largest subnet this canister could sit on, because the surplus comes
/// back and a shortfall shrinks the budget the call runs in.
#[test]
fn the_attached_cycles_grow_with_the_request_and_the_cap() {
    let small = cycles_for(100, 1_000);
    assert!(small > 0);
    assert!(cycles_for(100, 2_000) > small, "a larger cap reserves more");
    assert!(
        cycles_for(1_000, 1_000) > small,
        "a larger request reserves more"
    );
    // the measured version 1 price of a 3 kB call was 87.5M cycles; a version 2
    // reservation for the same cap covers the delivery of every byte of it
    assert!(
        cycles_for(1_000, 3_000) > 87_500_000,
        "the reservation is not below what the call can cost"
    );
}
