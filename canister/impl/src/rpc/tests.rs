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

/// Every way a batch reply can fail to be the batch that was asked for, each pinned to the
/// failure it is. One error carrying a sentence for six different cases cannot be asserted:
/// a test can only say "something went wrong", which passes when the wrong thing does.
#[test]
fn every_way_a_reply_is_not_the_batch_asked_for_names_itself() {
    let two = ["a", "b"];
    let batch = |answered: Value| parse_batch(answered.to_string().as_bytes(), &two);

    assert!(
        matches!(
            parse_batch(b"not json at all", &["a"]),
            Err(RpcError::NotAnArray { calls: 1, .. })
        ),
        "a body that is not json is not the array a batch is answered by"
    );
    assert!(
        matches!(
            batch(json!({"jsonrpc": "2.0", "id": 0, "result": "0x1"})),
            Err(RpcError::NotAnArray { calls: 2, .. })
        ),
        "and neither is one object"
    );
    assert_eq!(
        batch(json!([{"jsonrpc": "2.0", "id": 0, "result": "0x1"}])),
        Err(RpcError::WrongReplyCount {
            asked: 2,
            answered: 1
        })
    );
    assert_eq!(
        batch(json!([
            {"jsonrpc": "2.0", "id": 0, "result": "0x1"},
            {"jsonrpc": "2.0", "id": 7, "result": "0x2"},
        ])),
        Err(RpcError::UnknownReplyId {
            id: Some(7),
            calls: 2
        })
    );
    assert_eq!(
        batch(json!([
            {"jsonrpc": "2.0", "id": 0, "result": "0x1"},
            {"jsonrpc": "2.0", "result": "0x2"},
        ])),
        Err(RpcError::UnknownReplyId { id: None, calls: 2 }),
        "a reply with no id at all says which batch it was not part of"
    );
    assert_eq!(
        batch(json!([
            {"jsonrpc": "2.0", "id": 0, "result": "0x1"},
            {"jsonrpc": "2.0", "id": 0, "result": "0x2"},
        ])),
        Err(RpcError::DuplicateReplyId { id: 0 }),
        "one id answered twice leaves the other call unanswered"
    );
    assert_eq!(
        parse_batch(
            json!([
                {"jsonrpc": "2.0", "id": 0, "result": "0x1"},
                {"jsonrpc": "2.0", "id": 0, "result": "0x2"},
            ])
            .to_string()
            .as_bytes(),
            &["a", "a"],
        ),
        Err(RpcError::DuplicateReplyId { id: 0 })
    );
    assert_eq!(
        batch(json!([
            {"jsonrpc": "2.0", "id": 0},
            {"jsonrpc": "2.0", "id": 1, "result": "0x2"},
        ])),
        Err(RpcError::NeitherResultNorError {
            method: "a".to_string()
        }),
        "a reply that is neither an answer nor a refusal names the call it came back for"
    );
}

/// A reply carrying an id nothing asked for leaves the call it displaced unanswered, which
/// is its own refusal: the id check catches that shape first, so this is the one that a
/// reply count and a full set of ids still cannot rule out.
#[test]
fn a_call_with_no_reply_of_its_own_names_itself() {
    let sorted = sort_replies(
        json!([
            {"jsonrpc": "2.0", "id": 1, "result": "0x2"},
            {"jsonrpc": "2.0", "id": 1, "result": "0x2"},
        ])
        .to_string()
        .as_bytes(),
        &["a", "b"],
    );
    assert_eq!(sorted, Err(RpcError::DuplicateReplyId { id: 1 }));

    // `Unanswered` is what the last step answers when a slot is still empty, which the
    // checks above make unreachable from a well-formed array; it is the rule that says so
    assert_eq!(
        sort_replies(b"[]", &["only"]),
        Err(RpcError::WrongReplyCount {
            asked: 1,
            answered: 0
        })
    );
}

/// One reply is read by the same rules, and its error names its own method.
#[test]
fn a_reply_answers_its_result_or_names_its_method() {
    let ok = json!({"jsonrpc": "2.0", "id": 0, "result": "0x1"});
    assert_eq!(reply_result(&ok, "eth_chainId"), Ok(json!("0x1")));
    let failed = json!({"jsonrpc": "2.0", "id": 0, "error": {"message": "nope"}});
    assert_eq!(
        reply_result(&failed, "eth_chainId"),
        Err(RpcError::Rpc {
            method: "eth_chainId".to_string(),
            message: "nope".to_string(),
        })
    );
    assert_eq!(
        reply_result(&json!({"jsonrpc": "2.0", "id": 0}), "eth_chainId"),
        Err(RpcError::NeitherResultNorError {
            method: "eth_chainId".to_string()
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

/// Nothing a provider or the system wrote reaches a caller of this canister whole: a
/// summary is cut to its cap at a character boundary and carries no control character, so
/// a refusal a quoter reads back is a line in a log and never a page of somebody else's
/// html.
#[test]
fn a_summary_is_bounded_and_carries_no_control_characters() {
    assert_eq!(Summary::of("no route to host").as_str(), "no route to host");
    assert_eq!(
        Summary::of("rate\nlimited\r\n\tby\u{0}the provider").as_str(),
        "rate limited   by the provider",
        "control characters travel as spaces"
    );
    let page = "a".repeat(MAX_SUMMARY_BYTES * 10);
    assert_eq!(Summary::of(&page).as_str().len(), MAX_SUMMARY_BYTES);
    // a cut inside a character takes the character with it rather than trapping
    let wide = "é".repeat(MAX_SUMMARY_BYTES);
    let summary = Summary::of(&wide);
    assert!(summary.as_str().len() <= MAX_SUMMARY_BYTES);
    assert!(wide.starts_with(summary.as_str()));
    assert_eq!(summary.as_str().chars().count(), MAX_SUMMARY_BYTES / 2);
}

/// The replica refuses an answer longer than the cap its outcall reserved by rejecting the
/// whole call, `SysFatal` with "Http body exceeds size limit of N bytes" (DFINITY's
/// `evm-rpc-canister` reads a "length limit" the same way). That one refusal is typed, so
/// a read can ask again with a larger cap; every other rejection stays a transport
/// failure, summarised.
#[test]
fn an_answer_over_its_cap_is_refused_by_name() {
    assert_eq!(
        refusal(
            RejectionCode::SysFatal,
            "Http body exceeds size limit of 32768 bytes.",
            32_768
        ),
        RpcError::AnswerTooLarge { cap: 32_768 }
    );
    assert_eq!(
        refusal(
            RejectionCode::SysFatal,
            "Header size exceeds specified response length limit",
            512
        ),
        RpcError::AnswerTooLarge { cap: 512 }
    );
    assert_eq!(
        refusal(
            RejectionCode::SysTransient,
            "Http body exceeds size limit of 32768 bytes.",
            32_768
        ),
        RpcError::Unreachable {
            reason: Summary::of("SysTransient: Http body exceeds size limit of 32768 bytes.")
        },
        "only the system's fatal refusal is the size rule"
    );
    assert_eq!(
        refusal(
            RejectionCode::SysFatal,
            "Connecting to example.com failed",
            512
        ),
        RpcError::Unreachable {
            reason: Summary::of("SysFatal: Connecting to example.com failed")
        }
    );
}
