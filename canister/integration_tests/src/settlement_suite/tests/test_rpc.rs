//! What an unreplicated JSON-RPC batch looks like from outside the canister: one outcall
//! per batch, whatever the number of calls, and the replies read back in call order however
//! the provider ordered them.
//!
//! The two fields that make the outcall policy (`is_replicated: false`,
//! `pricing_version: 2`) are not observable here: pocket-ic 9 reports a pending outcall's
//! url, method, headers, body and cap, and nothing about its replication or pricing. They
//! are asserted where the request is built, in `impl/src/rpc`.

use crate::client::settlement::{set_config, test_rpc_batch};
use crate::settlement_suite::init::setup;
use candid::{encode_args, Principal};
use pocket_ic::common::rest::{
    CanisterHttpMethod, CanisterHttpReply, CanisterHttpResponse, MockCanisterHttpResponse,
};
use pocket_ic::PocketIc;
use serde_json::{json, Value};
use settlement_api::types::config::Config;
use settlement_api::types::errors::TestRpcError;
use settlement_api::types::rpc::RpcError;
use std::collections::BTreeMap;

const BASE: u64 = 8453;
const ARBITRUM: u64 = 42161;
const MAX_BYTES: u64 = 4_096;

/// A config with a provider for Base and none for Arbitrum.
fn with_base_rpc() -> Config {
    Config {
        rpc_urls: BTreeMap::from([(BASE, "https://base-mainnet.example/v2/key".to_string())]),
        ..Config::default()
    }
}

/// Submits the test door's batch without executing it, then gives the canister the round it
/// needs to reach the outcall.
fn submit_batch(
    pic: &PocketIc,
    canister: Principal,
    admin: Principal,
    chain_id: u64,
    methods: &[&str],
) -> pocket_ic::common::rest::RawMessageId {
    let methods: Vec<String> = methods.iter().map(|m| m.to_string()).collect();
    let id = pic
        .submit_call(
            canister,
            admin,
            "test_rpc_batch",
            encode_args((chain_id, methods, MAX_BYTES)).unwrap(),
        )
        .expect("the door accepts the call");
    // one round runs the message, and the outcall it makes is retrievable only after the
    // round following it
    pic.tick();
    pic.tick();
    id
}

fn answer(pic: &PocketIc, request: &pocket_ic::common::rest::CanisterHttpRequest, body: Value) {
    pic.mock_canister_http_response(MockCanisterHttpResponse {
        subnet_id: request.subnet_id,
        request_id: request.request_id,
        response: CanisterHttpResponse::CanisterHttpReply(CanisterHttpReply {
            status: 200,
            headers: vec![],
            body: body.to_string().into_bytes(),
        }),
        additional_responses: vec![],
    });
}

fn decode(raw: Vec<u8>) -> Result<Vec<String>, TestRpcError> {
    candid::decode_one(&raw).expect("the door answers its own type")
}

/// Two reads, one request: the body is a two-element array numbered in call order, and the
/// replies come back in that order although the provider answered them backwards.
#[test]
fn a_batch_of_two_methods_buys_exactly_one_outcall() {
    let (pic, canister, admin) = setup();
    set_config(&pic, canister, admin, &with_base_rpc()).unwrap();

    let call = submit_batch(
        &pic,
        canister,
        admin,
        BASE,
        &["eth_blockNumber", "eth_gasPrice"],
    );
    let pending = pic.get_canister_http();
    assert_eq!(pending.len(), 1, "two calls, one outcall");
    let request = &pending[0];
    assert_eq!(request.http_method, CanisterHttpMethod::POST);
    assert_eq!(request.url, "https://base-mainnet.example/v2/key");
    assert_eq!(request.max_response_bytes, Some(MAX_BYTES));
    let sent: Value = serde_json::from_slice(&request.body).expect("the body is json");
    assert_eq!(
        sent,
        json!([
            {"jsonrpc": "2.0", "id": 0, "method": "eth_blockNumber", "params": []},
            {"jsonrpc": "2.0", "id": 1, "method": "eth_gasPrice", "params": []},
        ])
    );

    // answered out of order: the ids are the order, the array is not
    answer(
        &pic,
        request,
        json!([
            {"jsonrpc": "2.0", "id": 1, "result": "0x3b9aca00"},
            {"jsonrpc": "2.0", "id": 0, "result": "0x1221e40"},
        ]),
    );
    assert_eq!(
        decode(pic.await_call(call).expect("the batch returns")),
        Ok(vec![
            "\"0x1221e40\"".to_string(),
            "\"0x3b9aca00\"".to_string()
        ])
    );
}

/// An error member is the provider answering one call of the batch, so the refusal names
/// which call it was.
#[test]
fn a_json_rpc_error_names_the_method_that_answered_it() {
    let (pic, canister, admin) = setup();
    set_config(&pic, canister, admin, &with_base_rpc()).unwrap();

    let call = submit_batch(
        &pic,
        canister,
        admin,
        BASE,
        &["eth_blockNumber", "eth_getLogs"],
    );
    let pending = pic.get_canister_http();
    answer(
        &pic,
        &pending[0],
        json!([
            {"jsonrpc": "2.0", "id": 0, "result": "0x1"},
            {"jsonrpc": "2.0", "id": 1, "error": {"code": -32000, "message": "header not found"}},
        ]),
    );
    assert_eq!(
        decode(pic.await_call(call).expect("the batch returns")),
        Err(TestRpcError::Rpc(RpcError::Rpc {
            method: "eth_getLogs".to_string(),
            message: "header not found".to_string(),
        }))
    );
}

/// A chain with no provider is a deploy mistake, caught before any cycles are spent: the
/// refusal names the chain and no outcall is pending.
#[test]
fn a_chain_with_no_rpc_url_buys_no_outcall() {
    let (pic, canister, admin) = setup();
    set_config(&pic, canister, admin, &with_base_rpc()).unwrap();

    let call = submit_batch(&pic, canister, admin, ARBITRUM, &["eth_blockNumber"]);
    assert!(
        pic.get_canister_http().is_empty(),
        "a missing url spends nothing"
    );
    assert_eq!(
        decode(pic.await_call(call).expect("the batch returns")),
        Err(TestRpcError::Rpc(RpcError::NoUrl { chain_id: ARBITRUM }))
    );
}

/// A provider that answers anything but a 2xx is a transport failure, and the status it
/// answered is what ops need to see.
#[test]
fn a_provider_answering_an_error_status_is_a_transport_failure() {
    let (pic, canister, admin) = setup();
    set_config(&pic, canister, admin, &with_base_rpc()).unwrap();

    let call = submit_batch(&pic, canister, admin, BASE, &["eth_blockNumber"]);
    let pending = pic.get_canister_http();
    pic.mock_canister_http_response(MockCanisterHttpResponse {
        subnet_id: pending[0].subnet_id,
        request_id: pending[0].request_id,
        response: CanisterHttpResponse::CanisterHttpReply(CanisterHttpReply {
            status: 429,
            headers: vec![],
            body: b"rate limited".to_vec(),
        }),
        additional_responses: vec![],
    });
    assert_eq!(
        decode(pic.await_call(call).expect("the batch returns")),
        Err(TestRpcError::Rpc(RpcError::Http {
            status: 429,
            body: "rate limited".to_string(),
        }))
    );
}

/// The door is the controller's, like every other test-only door.
#[test]
fn the_rpc_door_refuses_a_stranger() {
    let (pic, canister, admin) = setup();
    set_config(&pic, canister, admin, &with_base_rpc()).unwrap();
    let stranger = Principal::from_slice(&[9; 29]);
    let answer = test_rpc_batch(
        &pic,
        canister,
        stranger,
        BASE,
        &["eth_blockNumber".to_string()],
        MAX_BYTES,
    );
    assert!(matches!(answer, Err(TestRpcError::Guard(_))), "{answer:?}");
    assert!(pic.get_canister_http().is_empty());
}

/// M-03: the reads that decide money alone are made by every node of the subnet and agreed
/// on. Each node's answer goes through the canister's transform (a node's provider writes
/// its own headers and fields, and heads a moment apart are rounded to one), every node
/// reads at one block named by the agreed head, never `latest`, and an answer the nodes do
/// not agree on is no answer at all.
mod agreed {
    use super::*;
    use crate::settlement_suite::tests::test_pool::base_chain;
    use pocket_ic::common::rest::{CanisterHttpHeader, CanisterHttpRequest};

    const PORTAL: &str = "0xEC000769A73b70e16f361a442292500b3BCf4A85";
    /// Heads every node of the subnet sees a block or so apart: all round down to this.
    const HEAD: u64 = 0x1ec5_5b90;
    /// Base's depth in this config.
    const DEPTH: u64 = 2;

    fn agreed_config() -> Config {
        Config {
            confirmations: BTreeMap::from([(BASE, DEPTH)]),
            evm_chains: BTreeMap::from([(BASE, base_chain())]),
            eco_portal: Some(PORTAL.to_string()),
            ..with_base_rpc()
        }
    }

    fn nodes(pic: &PocketIc, canister: Principal) -> usize {
        let subnet = pic.get_subnet(canister).expect("the canister has a subnet");
        pic.topology().subnet_configs[&subnet].node_ids.len()
    }

    fn word(last: u8) -> String {
        format!("0x{}{last:02x}", "00".repeat(31))
    }

    /// The one pending outcall, its calls' methods and params.
    fn pending(pic: &PocketIc) -> (CanisterHttpRequest, Value) {
        let pending = pic.get_canister_http();
        assert_eq!(pending.len(), 1, "one outcall at a time");
        let body = serde_json::from_slice(&pending[0].body).expect("the body is json");
        (pending[0].clone(), body)
    }

    /// Answers `request` with `bodies[i]` from node i, each with a header of its own.
    fn answer_each(pic: &PocketIc, request: &CanisterHttpRequest, bodies: Vec<Value>) {
        let mut responses = bodies.into_iter().enumerate().map(|(node, body)| {
            CanisterHttpResponse::CanisterHttpReply(CanisterHttpReply {
                status: 200,
                headers: vec![CanisterHttpHeader {
                    name: "x-request-id".to_string(),
                    value: format!("node {node}"),
                }],
                body: body.to_string().into_bytes(),
            })
        });
        let response = responses.next().expect("one node at least");
        pic.mock_canister_http_response(MockCanisterHttpResponse {
            subnet_id: request.subnet_id,
            request_id: request.request_id,
            response,
            additional_responses: responses.collect(),
        });
        for _ in 0..6 {
            pic.tick();
        }
    }

    /// Every node's head, a block apart: the transform rounds them to [`HEAD`].
    fn answer_heads(pic: &PocketIc, n: usize) {
        let (request, body) = pending(pic);
        assert_eq!(body[0]["method"], "eth_blockNumber");
        let heads = (0..n as u64)
            .map(|node| json!([{"jsonrpc": "2.0", "id": 0, "result": format!("0x{:x}", HEAD + 1 + node)}]))
            .collect();
        answer_each(pic, &request, heads);
    }

    fn submit(
        pic: &PocketIc,
        canister: Principal,
        admin: Principal,
        method: &str,
        arg: Vec<u8>,
    ) -> pocket_ic::common::rest::RawMessageId {
        let id = pic
            .submit_call(canister, admin, method, arg)
            .expect("the door accepts the call");
        pic.tick();
        pic.tick();
        id
    }

    fn ran(
        pic: &PocketIc,
        canister: Principal,
        admin: Principal,
        used: impl Fn(usize) -> u8,
    ) -> Result<bool, String> {
        let n = nodes(pic, canister);
        let call = submit(
            pic,
            canister,
            admin,
            "test_agreed_ran",
            encode_args((BASE, vec![0x85_u8; 32])).unwrap(),
        );
        answer_heads(pic, n);
        let (request, body) = pending(pic);
        assert_eq!(body.as_array().unwrap().len(), 1);
        assert_eq!(body[0]["method"], "eth_call");
        assert_eq!(
            body[0]["params"][1],
            json!(format!("0x{:x}", HEAD - DEPTH)),
            "every node reads at the block the depth below the agreed head, never latest"
        );
        let answers = (0..n)
            .map(|node| json!([{"jsonrpc": "2.0", "id": 0, "result": word(used(node)), "node": node}]))
            .collect();
        answer_each(pic, &request, answers);
        candid::decode_one(&pic.await_call(call).expect("the door answers")).unwrap()
    }

    #[test]
    fn swapics_used_is_read_by_every_node_at_one_block_and_agreed() {
        let (pic, canister, admin) = setup();
        set_config(&pic, canister, admin, &agreed_config()).unwrap();
        assert_eq!(ran(&pic, canister, admin, |_| 1), Ok(true));
        assert_eq!(ran(&pic, canister, admin, |_| 0), Ok(false));
        // the nodes disagree (a provider or a node lies): no answer, asked again later
        let split = ran(&pic, canister, admin, |node| u8::from(node % 2 == 0));
        assert!(
            matches!(&split, Err(why) if why.contains("Unreachable")),
            "the system refuses an outcall its nodes do not agree on: {split:?}"
        );
    }

    fn fill(
        pic: &PocketIc,
        canister: Principal,
        admin: Principal,
        timestamp: u64,
        after_s: u64,
    ) -> Result<(String, u64), String> {
        let n = nodes(pic, canister);
        let call = submit(
            pic,
            canister,
            admin,
            "test_agreed_fill",
            encode_args((BASE, vec![0x84_u8; 32], after_s)).unwrap(),
        );
        answer_heads(pic, n);
        let (request, body) = pending(pic);
        let block = json!(format!("0x{:x}", HEAD - DEPTH));
        assert_eq!(body[0]["method"], "eth_getBlockByNumber");
        assert_eq!(body[0]["params"], json!([block, false]));
        assert_eq!(body[1]["method"], "eth_call");
        assert_eq!(body[1]["params"][1], block);
        let answers = (0..n)
            .map(|node| {
                json!([
                    {"jsonrpc": "2.0", "id": 0, "result": {
                        "number": block, "hash": format!("0x{}", "ab".repeat(32)),
                        "timestamp": format!("0x{timestamp:x}"), "miner": format!("node {node}"),
                    }},
                    {"jsonrpc": "2.0", "id": 1, "result": word(0)},
                ])
            })
            .collect();
        answer_each(pic, &request, answers);
        candid::decode_one(&pic.await_call(call).expect("the door answers")).unwrap()
    }

    /// L-03: "not filled" only at an agreed block past the route's deadline.
    #[test]
    fn an_eco_fill_is_read_by_every_node_at_a_block_past_the_deadline() {
        let (pic, canister, admin) = setup();
        set_config(&pic, canister, admin, &agreed_config()).unwrap();
        let deadline = 1_800_000_000;
        assert_eq!(
            fill(&pic, canister, admin, deadline + 2, deadline),
            Ok(("Open".to_string(), HEAD - DEPTH))
        );
        assert_eq!(
            fill(&pic, canister, admin, deadline, deadline),
            Err("TooEarly".to_string())
        );
    }
}
