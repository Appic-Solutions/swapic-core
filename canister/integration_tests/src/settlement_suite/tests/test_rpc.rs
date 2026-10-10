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
/// answered is what the operator needs to see.
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
