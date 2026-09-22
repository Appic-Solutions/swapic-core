//! The nonce allocator, the sign-before-send order, and the outbox that gets the bytes
//! onto a chain.
//!
//! Every test here runs on a network holding pocket-ic's test threshold keys, because a
//! transaction that is not signed is not a transaction.

use crate::client::settlement::{
    append, derive_evm_address, events_page, evm_address, push_chain_data, set_halted,
    test_outbox_armed, test_send, verify_replay,
};
use crate::settlement_suite::init::{install, quoter, upgrade, watcher};
use candid::{encode_args, Nat, Principal};
use pocket_ic::common::rest::{
    CanisterHttpReject, CanisterHttpReply, CanisterHttpRequest, CanisterHttpResponse,
    MockCanisterHttpResponse,
};
use pocket_ic::{PocketIc, PocketIcBuilder};
use serde_json::{json, Value};
use settlement_api::types::chain_data::ChainData;
use settlement_api::types::config::Config;
use settlement_api::types::errors::{AppendError, TestAppendError};
use settlement_api::types::events::{Event, EventType, Hash32, TxPurpose};
use settlement_api::types::init::InitArg;
use settlement_api::types::swap::TransitionError;
use settlement_api::types::tx::TxError;
use std::collections::BTreeMap;
use std::time::Duration;
use types::{ChainId, GasMode, Rail, TokenAmount, UnixSeconds};

const BASE: u64 = 8453;
const VAULT: &str = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913";
const GAS_LIMIT: u64 = 120_000;

/// The batch window the default config is on, which is how long a queued transaction waits
/// before the outbox pass takes it.
const BATCH_WINDOW: Duration = Duration::from_secs(2);

/// How long an allocated nonce waits for its signed record before the pass cancels it: the
/// canister's `STRANDED_AFTER`, five minutes, which is a signing round trip and then some
/// and never the batch window.
const STRANDED_AFTER: Duration = Duration::from_secs(300);

/// A quote whose swap the tests send transactions for, one per nonce.
fn quote(nonce: u64) -> types::Quote {
    types::Quote {
        version: 1,
        src_chain: ChainId::BASE,
        src_token: "USDC".parse().unwrap(),
        amount_in: TokenAmount::from(1_000_u32),
        dst_chain: ChainId::ARBITRUM,
        dst_token: "USDC".parse().unwrap(),
        expected_out: TokenAmount::from(999_u32),
        min_out: TokenAmount::from(990_u32),
        dst_address: "0xuser".parse().unwrap(),
        refund_address: None,
        auto_refund: true,
        gas_mode: GasMode::Gasless,
        rail: Rail::CctpV2Fast,
        expires_at: UnixSeconds::new(1_800_000_000),
        nonce,
    }
}

fn swap_id(nonce: u64) -> Hash32 {
    quote(nonce)
        .hash()
        .expect("a valid quote has a preimage")
        .into_bytes()
}

fn funds(nonce: u64) -> EventType {
    let quote = quote(nonce);
    EventType::FundsReceived {
        quote_hash: swap_id(nonce),
        quote_bytes: quote
            .canonical_bytes()
            .expect("a valid quote has a preimage"),
        chain_id: BASE,
        token: quote.src_token.to_string(),
        amount: quote.amount_in.into(),
        tx_ref: "0xfeed".into(),
    }
}

/// A canister on a network holding the test threshold keys, with a provider for Base, a
/// fresh chain reading, and `count` funded swaps ready to send from.
fn setup_with_swaps(count: u64) -> (PocketIc, Principal, Principal) {
    let pic = PocketIcBuilder::new()
        .with_ii_subnet()
        .with_application_subnet()
        .build();
    let admin = Principal::from_slice(&[1; 29]);
    let subnet = pic.topology().get_app_subnets()[0];
    let canister = pic.create_canister_on_subnet(Some(admin), None, subnet);
    pic.add_cycles(canister, 1_000_000_000_000_000);
    let arg = InitArg {
        config: Config {
            rpc_urls: BTreeMap::from([(BASE, "https://base-mainnet.example/v2/key".to_string())]),
            ..Config::default()
        },
        quoter: quoter(),
        watcher: watcher(),
    };
    install(&pic, canister, admin, &arg).expect("the arg installs");
    derive_evm_address(&pic, canister, admin).expect("the test key derives an address");
    push_reading(&pic, canister);
    for nonce in 0..count {
        append(&pic, canister, admin, &funds(nonce)).expect("the swap is funded");
    }
    (pic, canister, admin)
}

/// Keeps the chain reading young: a transaction is priced from it, and the default
/// `chain_data_max_age` is ten seconds.
fn push_reading(pic: &PocketIc, canister: Principal) {
    push_chain_data(
        pic,
        canister,
        watcher(),
        BASE,
        &ChainData {
            block: 19_000_000,
            base_fee_wei_per_gas: Nat::from(1_000_000_000_u64),
            priority_fee_wei_per_gas: Nat::from(100_000_000_u64),
        },
    )
    .expect("the watcher may push");
}

fn send(
    pic: &PocketIc,
    canister: Principal,
    admin: Principal,
    nonce: u64,
) -> Result<Hash32, TxError> {
    test_send(pic, canister, admin, swap_id(nonce), BASE, VAULT, GAS_LIMIT)
}

fn events(pic: &PocketIc, canister: Principal) -> Vec<Event> {
    events_page(pic, canister, Principal::anonymous(), 0, 500)
}

/// Moves the clock, keeps the chain reading young at the new time the way a live watcher
/// would, then gives the canister the rounds a one-shot pass and its outcall need.
fn advance(pic: &PocketIc, canister: Principal, by: Duration) {
    pic.advance_time(by);
    push_reading(pic, canister);
    for _ in 0..4 {
        pic.tick();
    }
}

/// The methods one pending outcall asks for, in order.
fn methods(request: &CanisterHttpRequest) -> Vec<String> {
    let body: Value = serde_json::from_slice(&request.body).expect("the body is json");
    body.as_array()
        .expect("a batch is an array")
        .iter()
        .map(|call| {
            call["method"]
                .as_str()
                .expect("a call names a method")
                .to_string()
        })
        .collect()
}

/// The parameters of the `n`th call of one pending outcall.
fn params(request: &CanisterHttpRequest, n: usize) -> Value {
    let body: Value = serde_json::from_slice(&request.body).expect("the body is json");
    body[n]["params"].clone()
}

fn reply(pic: &PocketIc, request: &CanisterHttpRequest, results: Vec<Value>) {
    let body: Vec<Value> = results
        .into_iter()
        .enumerate()
        .map(|(id, result)| json!({"jsonrpc": "2.0", "id": id, "result": result}))
        .collect();
    pic.mock_canister_http_response(MockCanisterHttpResponse {
        subnet_id: request.subnet_id,
        request_id: request.request_id,
        response: CanisterHttpResponse::CanisterHttpReply(CanisterHttpReply {
            status: 200,
            headers: vec![],
            body: Value::Array(body).to_string().into_bytes(),
        }),
        additional_responses: vec![],
    });
    // the pass continues where its await left off, and what it does next may be another
    // await of its own: a signature takes several rounds. An outcall left unanswered
    // across a clock jump times out, so every answer is delivered, and worked through,
    // before the test moves on.
    for _ in 0..12 {
        pic.tick();
    }
}

/// Answers whatever the pending outcall asks: a hash for every broadcast, the head block
/// and `receipt` for every receipt asked for.
fn answer_pending(pic: &PocketIc, latest: u64, receipt: &Value) -> Vec<String> {
    let pending = pic.get_canister_http();
    let mut asked = Vec::new();
    for request in pending {
        let asked_for = methods(&request);
        let results = asked_for
            .iter()
            .map(|method| match method.as_str() {
                "eth_blockNumber" => json!(format!("0x{latest:x}")),
                "eth_getTransactionReceipt" => receipt.clone(),
                _ => json!("0x1111111111111111111111111111111111111111111111111111111111111111"),
            })
            .collect();
        reply(pic, &request, results);
        asked.extend(asked_for);
    }
    asked
}

fn receipt(block: u64, success: bool, tx_hash: Hash32) -> Value {
    json!({
        "transactionHash": format!("0x{}", hex::encode(tx_hash)),
        // a receipt names the block it is in as well as the height of it, and a record
        // without one was mined into no block at all
        "blockHash": format!("0x{}", hex::encode([block as u8; 32])),
        "blockNumber": format!("0x{block:x}"),
        "status": if success { "0x1" } else { "0x0" },
    })
}

/// A `TxCreated` for the first swap at `nonce`, which the allocator admits only at the
/// number it is at.
fn created_at(nonce: u64) -> EventType {
    created_for(0, nonce)
}

/// A `TxCreated` for the swap of the quote at `swap`, at `nonce`.
fn created_for(swap: u64, nonce: u64) -> EventType {
    EventType::TxCreated {
        purpose: TxPurpose::Payout(swap_id(swap)),
        chain_id: BASE,
        nonce,
        to: VAULT.to_string(),
        value_wei: Nat::from(0_u8),
        data: vec![],
        gas_limit: Nat::from(GAS_LIMIT),
        max_fee_wei_per_gas: Nat::from(1_u8),
        max_priority_fee_wei_per_gas: Nat::from(1_u8),
    }
}

fn tx_created(events: &[Event]) -> Vec<(u64, u64)> {
    events
        .iter()
        .filter_map(|event| match &event.payload {
            EventType::TxCreated {
                chain_id, nonce, ..
            } => Some((*chain_id, *nonce)),
            _ => None,
        })
        .collect()
}

fn tx_signed(events: &[Event]) -> Vec<Vec<u8>> {
    events
        .iter()
        .filter_map(|event| match &event.payload {
            EventType::TxSigned { raw_tx, .. } => Some(raw_tx.clone()),
            _ => None,
        })
        .collect()
}

/// Every payload of `kind` in the log, where `kind` says which variant to keep.
fn of_kind(events: Vec<Event>, kind: fn(&EventType) -> bool) -> Vec<EventType> {
    events
        .into_iter()
        .map(|event| event.payload)
        .filter(kind)
        .collect()
}

fn is_confirmed(payload: &EventType) -> bool {
    matches!(payload, EventType::TxConfirmed { .. })
}

fn is_failed(payload: &EventType) -> bool {
    matches!(payload, EventType::TxFailed { .. })
}

fn is_replaced(payload: &EventType) -> bool {
    matches!(payload, EventType::TxReplaced { .. })
}

/// The test rule A4 exists for: two sends that are both in flight at the signature come
/// back with two different nonces, and the log holds no two `TxCreated` at one nonce.
/// `TxCreated` is appended before the first await, so the interleaving cannot reach it.
#[test]
fn two_sends_that_interleave_at_the_signature_get_different_nonces() {
    let (pic, canister, admin) = setup_with_swaps(2);
    let first = pic
        .submit_call(
            canister,
            admin,
            "test_send",
            encode_args((swap_id(0), BASE, VAULT.to_string(), GAS_LIMIT)).unwrap(),
        )
        .unwrap();
    let second = pic
        .submit_call(
            canister,
            admin,
            "test_send",
            encode_args((swap_id(1), BASE, VAULT.to_string(), GAS_LIMIT)).unwrap(),
        )
        .unwrap();
    // enough rounds for both messages to run to their signature and back: the first takes
    // both of them past the append that allocates, which is the interleaving under test
    for _ in 0..11 {
        pic.tick();
    }
    let first: Result<Hash32, TxError> =
        candid::decode_one(&pic.await_call(first).expect("the first send returns")).unwrap();
    let second: Result<Hash32, TxError> =
        candid::decode_one(&pic.await_call(second).expect("the second send returns")).unwrap();
    assert!(first.is_ok(), "{first:?}");
    assert!(second.is_ok(), "{second:?}");
    assert_ne!(first, second, "two transactions, two hashes");

    let created = tx_created(&events(&pic, canister));
    assert_eq!(
        created,
        vec![(BASE, 0), (BASE, 1)],
        "the allocator handed out one number each"
    );
}

/// Rule A4 from the swap's side, which is what the guard on `TxCreated` exists for: two
/// sends for ONE swap that are both in flight at the signature allocate once. The second
/// reaches the append while the first is still holding its number unsigned, and is refused
/// there, before it can take a number that no `TxSigned` could ever spend. Without the
/// guard both allocate, the second's signed record is refused because the first attempt is
/// open, and its number is stranded.
#[test]
fn two_sends_for_one_swap_that_interleave_at_the_signature_allocate_once() {
    let (pic, canister, admin) = setup_with_swaps(1);
    let same_swap = || encode_args((swap_id(0), BASE, VAULT.to_string(), GAS_LIMIT)).unwrap();
    let first = pic
        .submit_call(canister, admin, "test_send", same_swap())
        .unwrap();
    let second = pic
        .submit_call(canister, admin, "test_send", same_swap())
        .unwrap();
    for _ in 0..11 {
        pic.tick();
    }
    let first: Result<Hash32, TxError> =
        candid::decode_one(&pic.await_call(first).expect("the first send returns")).unwrap();
    let second: Result<Hash32, TxError> =
        candid::decode_one(&pic.await_call(second).expect("the second send returns")).unwrap();
    assert!(first.is_ok(), "{first:?}");
    assert_eq!(
        second,
        Err(TxError::Append(AppendError::Transition(
            TransitionError::NonceStillUnsigned(swap_id(0))
        ))),
        "the swap was holding its number unsigned when the second send reached the append"
    );

    let log = events(&pic, canister);
    assert_eq!(
        tx_created(&log),
        vec![(BASE, 0)],
        "one number was handed out"
    );
    assert_eq!(
        tx_signed(&log).len(),
        1,
        "and the one transaction that carries it was signed"
    );

    // nothing is stranded: the first send spent its number, so the pass that ends
    // abandoned allocations has nothing to cancel, and the outbox holds one transaction
    advance(&pic, canister, BATCH_WINDOW);
    let pending = pic.get_canister_http();
    assert_eq!(pending.len(), 1, "one chain, one batch");
    assert_eq!(methods(&pending[0]), vec!["eth_sendRawTransaction"]);
    reply(&pic, &pending[0], vec![json!("0xabc")]);
    assert!(
        of_kind(events(&pic, canister), is_cancelled).is_empty(),
        "nothing was stranded, so nothing was cancelled"
    );
    assert!(
        verify_replay(&pic, canister, Principal::anonymous()),
        "the log still folds to the live fold"
    );
}

/// The guard, from the outside: a `TxCreated` carrying anything but the number the
/// allocator is at is refused, and it allocates nothing.
#[test]
fn a_transaction_created_off_the_allocator_is_refused() {
    let (pic, canister, admin) = setup_with_swaps(1);
    let refused = append(&pic, canister, admin, &created_at(1));
    assert!(refused.is_err(), "{refused:?}");
    assert!(
        tx_created(&events(&pic, canister)).is_empty(),
        "a refused allocation allocates nothing"
    );

    // and the number the allocator is at is accepted
    assert!(append(&pic, canister, admin, &created_at(0)).is_ok());
    assert_eq!(tx_created(&events(&pic, canister)), vec![(BASE, 0)]);
}

/// Rule A6: the signature is recorded before anything is broadcast, and what goes out is
/// what was recorded, byte for byte.
#[test]
fn the_bytes_broadcast_are_the_bytes_the_log_recorded() {
    let (pic, canister, admin) = setup_with_swaps(1);
    send(&pic, canister, admin, 0).expect("the send goes through");

    let signed = tx_signed(&events(&pic, canister));
    assert_eq!(signed.len(), 1, "one transaction, one signature");
    assert!(
        pic.get_canister_http().is_empty(),
        "nothing is broadcast before the signature is recorded"
    );

    advance(&pic, canister, BATCH_WINDOW);
    let pending = pic.get_canister_http();
    assert_eq!(pending.len(), 1, "one chain, one batch");
    assert_eq!(methods(&pending[0]), vec!["eth_sendRawTransaction"]);
    assert_eq!(
        params(&pending[0], 0),
        json!([format!("0x{}", hex::encode(&signed[0]))]),
        "the broadcast is the recorded bytes"
    );
}

/// A transaction nobody has mined goes out again unchanged: the same bytes, no new
/// signature, and no new line in the log.
#[test]
fn a_missing_receipt_rebroadcasts_the_same_bytes_with_no_new_signature() {
    let (pic, canister, admin) = setup_with_swaps(1);
    send(&pic, canister, admin, 0).expect("the send goes through");
    advance(&pic, canister, BATCH_WINDOW);
    let pending = pic.get_canister_http();
    reply(&pic, &pending[0], vec![json!("0xabc")]);

    let before = events(&pic, canister).len();
    let signed = tx_signed(&events(&pic, canister));

    // the next pass reads a receipt that is not there yet, and the one after the
    // rebroadcast window sends the same bytes again
    advance(&pic, canister, BATCH_WINDOW);
    let asked = answer_pending(&pic, 19_000_001, &json!(null));
    assert_eq!(
        asked,
        vec!["eth_blockNumber", "eth_getTransactionReceipt"],
        "the head and one receipt, in one batch"
    );

    advance(&pic, canister, Duration::from_secs(31));
    answer_pending(&pic, 19_000_002, &json!(null));
    advance(&pic, canister, BATCH_WINDOW);
    let pending = pic.get_canister_http();
    let sends: Vec<&CanisterHttpRequest> = pending
        .iter()
        .filter(|request| methods(request).contains(&"eth_sendRawTransaction".to_string()))
        .collect();
    assert_eq!(sends.len(), 1, "the transaction went out again");
    assert_eq!(
        params(sends[0], 0),
        json!([format!("0x{}", hex::encode(&signed[0]))]),
        "the same bytes"
    );
    assert_eq!(
        tx_signed(&events(&pic, canister)),
        signed,
        "a rebroadcast signs nothing"
    );
    assert_eq!(
        events(&pic, canister).len(),
        before,
        "a rebroadcast writes nothing to the log"
    );
}

/// A receipt is not a confirmation until it is deep enough, and at depth it closes the
/// attempt.
#[test]
fn a_receipt_closes_the_attempt_only_once_it_is_deep_enough() {
    let (pic, canister, admin) = setup_with_swaps(1);
    let tx_hash = send(&pic, canister, admin, 0).expect("the send goes through");
    advance(&pic, canister, BATCH_WINDOW);
    reply(&pic, &pic.get_canister_http()[0], vec![json!("0xabc")]);

    // mined in a block the head has not reached: two moments of the chain, not a
    // confirmation
    advance(&pic, canister, BATCH_WINDOW);
    answer_pending(&pic, 19_000_000, &receipt(19_000_005, true, tx_hash));
    assert!(
        of_kind(events(&pic, canister), is_confirmed).is_empty(),
        "nothing is confirmed by a receipt ahead of the head"
    );

    // Base is configured at a depth of one, so the receipt's own block is enough
    advance(&pic, canister, BATCH_WINDOW);
    answer_pending(&pic, 19_000_005, &receipt(19_000_005, true, tx_hash));
    let confirmed = of_kind(events(&pic, canister), is_confirmed);
    assert_eq!(confirmed.len(), 1, "the attempt closed exactly once");
    assert!(matches!(
        confirmed[0],
        EventType::TxConfirmed { block, .. } if block == 19_000_005
    ));

    // the entry left the outbox, so nothing is read for it again
    advance(&pic, canister, Duration::from_secs(300));
    assert!(
        pic.get_canister_http().is_empty(),
        "a closed attempt is no longer in the outbox"
    );
}

/// A transaction that reverted did not do what it was sent to do, and that closes the
/// attempt as a failure rather than a confirmation.
#[test]
fn a_reverted_receipt_fails_the_attempt() {
    let (pic, canister, admin) = setup_with_swaps(1);
    let tx_hash = send(&pic, canister, admin, 0).expect("the send goes through");
    advance(&pic, canister, BATCH_WINDOW);
    reply(&pic, &pic.get_canister_http()[0], vec![json!("0xabc")]);

    advance(&pic, canister, BATCH_WINDOW);
    answer_pending(&pic, 19_000_005, &receipt(19_000_005, false, tx_hash));
    assert_eq!(
        of_kind(events(&pic, canister), is_failed).len(),
        1,
        "the attempt failed exactly once"
    );
}

/// Rule A5: a transaction that is not landing is replaced at the same nonce with a higher
/// fee, and the replacement's receipt closes the attempt. The nonce is never abandoned.
#[test]
fn a_stuck_transaction_is_replaced_at_the_same_nonce_with_a_higher_fee() {
    let (pic, canister, admin) = setup_with_swaps(1);
    let first_hash = send(&pic, canister, admin, 0).expect("the send goes through");
    advance(&pic, canister, BATCH_WINDOW);
    reply(&pic, &pic.get_canister_http()[0], vec![json!("0xabc")]);

    // nothing mined yet, and the transaction is still young
    answer_pending(&pic, 19_000_000, &json!(null));

    // long past the stuck window, with nothing mined
    advance(&pic, canister, Duration::from_secs(200));
    answer_pending(&pic, 19_000_001, &json!(null));

    let replaced = of_kind(events(&pic, canister), is_replaced);
    assert_eq!(replaced.len(), 1, "one replacement");
    let EventType::TxReplaced {
        nonce,
        chain_id,
        max_fee_wei_per_gas,
        tx_hash: replacement_hash,
        ..
    } = replaced[0].clone()
    else {
        unreachable!()
    };
    assert_eq!((chain_id, nonce), (BASE, 0), "the same nonce");
    assert!(
        max_fee_wei_per_gas > 2_100_000_000_u64,
        "a replacement pays more than the transaction it replaces"
    );
    assert_ne!(
        replacement_hash, first_hash,
        "the replacement is another transaction"
    );
    assert_eq!(
        tx_created(&events(&pic, canister)),
        vec![(BASE, 0)],
        "a replacement allocates no nonce"
    );

    // the replacement goes out, and its receipt closes the attempt
    advance(&pic, canister, BATCH_WINDOW);
    let asked = answer_pending(&pic, 19_000_002, &json!(null));
    assert!(asked.contains(&"eth_sendRawTransaction".to_string()));

    advance(&pic, canister, BATCH_WINDOW);
    answer_pending(
        &pic,
        19_000_010,
        &receipt(19_000_010, true, replacement_hash),
    );
    assert_eq!(
        of_kind(events(&pic, canister), is_confirmed).len(),
        1,
        "the replacement's receipt closed it"
    );
}

/// Rule A9: the pass that sends a queued transaction lives in the heap, and an upgrade
/// clears it. A transaction signed before the upgrade still goes out after it.
#[test]
fn the_flush_pass_is_re_armed_after_an_upgrade_with_a_queued_entry() {
    let (pic, canister, admin) = setup_with_swaps(1);
    send(&pic, canister, admin, 0).expect("the send goes through");
    let signed = tx_signed(&events(&pic, canister));

    // upgraded before the window closes, so nothing has gone out yet
    assert!(pic.get_canister_http().is_empty());
    upgrade(&pic, canister, admin).expect("the upgrade goes through");

    advance(&pic, canister, BATCH_WINDOW);
    let pending = pic.get_canister_http();
    assert_eq!(pending.len(), 1, "the queued transaction went out anyway");
    assert_eq!(
        params(&pending[0], 0),
        json!([format!("0x{}", hex::encode(&signed[0]))]),
        "and it is the transaction the log recorded"
    );
}

/// A replacement is a new transaction, signed and broadcast, so the halt switch stops it
/// like every other path that creates one. What is already out there is still watched: the
/// receipts keep being read, because an operator investigating a divergence needs to see
/// what the chains did with what this canister already signed.
#[test]
fn a_halted_canister_replaces_nothing_and_still_reads_its_receipts() {
    let (pic, canister, admin) = setup_with_swaps(1);
    let tx_hash = send(&pic, canister, admin, 0).expect("the send goes through");
    advance(&pic, canister, BATCH_WINDOW);
    reply(&pic, &pic.get_canister_http()[0], vec![json!("0xabc")]);
    answer_pending(&pic, 19_000_000, &json!(null));

    set_halted(&pic, canister, admin, true).expect("a controller may halt");

    // long past the stuck window, with nothing mined
    advance(&pic, canister, Duration::from_secs(200));
    let asked = answer_pending(&pic, 19_000_001, &json!(null));
    assert!(
        asked.contains(&"eth_getTransactionReceipt".to_string()),
        "a halted canister still reads what it already sent"
    );
    assert!(
        of_kind(events(&pic, canister), is_replaced).is_empty(),
        "a halted canister signs no replacement"
    );
    assert_eq!(
        tx_created(&events(&pic, canister)),
        vec![(BASE, 0)],
        "and the nonce it allocated is still allocated"
    );

    // once the halt is lifted the replacement happens, so nothing was abandoned
    set_halted(&pic, canister, admin, false).expect("a controller may resume");
    advance(&pic, canister, BATCH_WINDOW);
    answer_pending(&pic, 19_000_002, &json!(null));
    assert_eq!(
        of_kind(events(&pic, canister), is_replaced).len(),
        1,
        "the transaction was waiting, not abandoned"
    );

    // and the receipt that closes the attempt still closes it: the replacement goes out
    // on the next pass, and the pass after it reads a receipt for every hash this nonce
    // has ever carried, so the one that landed is found whichever it was
    advance(&pic, canister, BATCH_WINDOW);
    answer_pending(&pic, 19_000_009, &json!(null));
    advance(&pic, canister, BATCH_WINDOW);
    answer_pending(&pic, 19_000_010, &receipt(19_000_010, true, tx_hash));
    assert_eq!(
        of_kind(events(&pic, canister), is_confirmed).len(),
        1,
        "the original transaction landed after all"
    );
}

fn is_cancelled(payload: &EventType) -> bool {
    matches!(payload, EventType::TxCancelled { .. })
}

fn tx_cancelled(events: &[Event]) -> Vec<(u64, u64, Vec<u8>)> {
    events
        .iter()
        .filter_map(|event| match &event.payload {
            EventType::TxCancelled {
                chain_id,
                nonce,
                raw_tx,
                ..
            } => Some((*chain_id, *nonce, raw_tx.clone())),
            _ => None,
        })
        .collect()
}

/// A canister with a provider and a fresh reading on a network that holds NO threshold key,
/// so the signature a send asks for is refused: the one failure that strands a nonce
/// without any test door planting it.
fn setup_without_a_signing_key(count: u64) -> (PocketIc, Principal, Principal) {
    let (pic, canister, admin) = crate::settlement_suite::init::empty_canister();
    let arg = InitArg {
        config: Config {
            rpc_urls: BTreeMap::from([(BASE, "https://base-mainnet.example/v2/key".to_string())]),
            ..Config::default()
        },
        quoter: quoter(),
        watcher: watcher(),
    };
    install(&pic, canister, admin, &arg).expect("the arg installs");
    push_reading(&pic, canister);
    for nonce in 0..count {
        append(&pic, canister, admin, &funds(nonce)).expect("the swap is funded");
    }
    (pic, canister, admin)
}

/// Rule A5, the failure it is really about: the signature is asked for AFTER the nonce is
/// allocated, so a management canister that refuses it leaves the number handed out with no
/// transaction carrying it. The fold holds that number, and the chain does not stop: the
/// next send takes the next one.
#[test]
fn a_refused_signature_leaves_the_nonce_unsigned_and_blocks_no_later_send() {
    let (pic, canister, admin) = setup_without_a_signing_key(2);
    let refused = send(&pic, canister, admin, 0);
    assert!(
        matches!(refused, Err(TxError::Ecdsa(_))),
        "a network with no key refuses the signature: {refused:?}"
    );
    assert_eq!(
        tx_created(&events(&pic, canister)),
        vec![(BASE, 0)],
        "the number was handed out before the signature was asked for"
    );
    assert!(
        tx_signed(&events(&pic, canister)).is_empty(),
        "and nothing was signed"
    );

    // the same swap is refused another number while it is still holding this one
    let twice = send(&pic, canister, admin, 0);
    assert!(
        matches!(twice, Err(TxError::Append(_))),
        "one swap holds one number: {twice:?}"
    );

    // a different swap is not blocked by it: the allocator moves on
    let other = send(&pic, canister, admin, 1);
    assert!(matches!(other, Err(TxError::Ecdsa(_))), "{other:?}");
    assert_eq!(
        tx_created(&events(&pic, canister)),
        vec![(BASE, 0), (BASE, 1)],
        "the next send took the next number"
    );
    assert!(
        verify_replay(&pic, canister, Principal::anonymous()),
        "the log still folds to the live fold"
    );
}

/// The repair rule A5 asks for: a number that was handed out and lost its transaction is
/// spent by a zero-value transfer from this canister's address to itself at that exact
/// number, recorded before it is broadcast like every other transaction. Without it the
/// account has a gap and can never mine anything above it again.
#[test]
fn an_allocation_that_lost_its_transaction_is_spent_by_a_cancel() {
    let (pic, canister, admin) = setup_with_swaps(1);
    let mine = evm_address(&pic, canister, Principal::anonymous()).expect("the address is derived");

    // a `TxCreated` with no `TxSigned` after it: exactly the state a refused signature, a
    // refused signed record or a trap mid-await leaves behind
    append(&pic, canister, admin, &created_at(0)).expect("the allocation is admitted");
    assert!(
        pic.get_canister_http().is_empty(),
        "nothing is broadcast before the cancel is recorded"
    );

    advance(&pic, canister, STRANDED_AFTER);
    let cancelled = tx_cancelled(&events(&pic, canister));
    assert_eq!(cancelled.len(), 1, "one cancel for one stranded number");
    let (chain_id, nonce, raw_tx) = cancelled[0].clone();
    assert_eq!((chain_id, nonce), (BASE, 0), "at the number it spends");
    let mine_hex = mine
        .strip_prefix("0x")
        .expect("an evm address is written with its prefix")
        .to_ascii_lowercase();
    let raw_hex = hex::encode(&raw_tx).to_ascii_lowercase();
    assert!(
        raw_hex.contains(&mine_hex),
        "a cancel sends to this canister's own address"
    );
    assert!(
        !raw_hex.contains(&VAULT[2..].to_ascii_lowercase()),
        "and not to the vault the stranded transaction was for"
    );

    // recorded first, then broadcast, like every other transaction (A6)
    let pending = pic.get_canister_http();
    let sends: Vec<&CanisterHttpRequest> = pending
        .iter()
        .filter(|request| methods(request).contains(&"eth_sendRawTransaction".to_string()))
        .collect();
    assert_eq!(sends.len(), 1, "the cancel went out");
    assert_eq!(
        params(sends[0], 0),
        json!([format!("0x{}", hex::encode(&raw_tx))]),
        "the bytes broadcast are the bytes the log recorded"
    );
    assert!(
        verify_replay(&pic, canister, Principal::anonymous()),
        "the log still folds to the live fold"
    );

    // and the number is spent, not given back: the next send takes the one after it
    reply(&pic, sends[0], vec![json!("0xabc")]);
    send(&pic, canister, admin, 0).expect("the swap may send again");
    assert_eq!(
        tx_created(&events(&pic, canister)),
        vec![(BASE, 0), (BASE, 1)],
        "the allocator moved on"
    );
}

/// The cancel rides the outbox the way any transaction does, and its receipt closes the
/// outbox entry and writes nothing: `TxCancelled` already sealed that number's fate before
/// the bytes went out, so what the chain did with them changes nothing in the fold.
#[test]
fn a_cancels_receipt_closes_its_entry_and_writes_no_line() {
    let (pic, canister, admin) = setup_with_swaps(1);
    append(&pic, canister, admin, &created_at(0)).expect("the allocation is admitted");
    advance(&pic, canister, STRANDED_AFTER);
    let cancelled = tx_cancelled(&events(&pic, canister));
    assert_eq!(cancelled.len(), 1);
    let cancel_hash: Hash32 = {
        let EventType::TxCancelled { tx_hash, .. } = events(&pic, canister)
            .into_iter()
            .map(|event| event.payload)
            .find(is_cancelled)
            .expect("the cancel is in the log")
        else {
            unreachable!()
        };
        tx_hash
    };
    let pending = pic.get_canister_http();
    reply(&pic, &pending[0], vec![json!("0xabc")]);

    let before = events(&pic, canister).len();
    advance(&pic, canister, BATCH_WINDOW);
    let asked = answer_pending(&pic, 19_000_005, &receipt(19_000_005, true, cancel_hash));
    assert!(asked.contains(&"eth_getTransactionReceipt".to_string()));
    assert_eq!(
        events(&pic, canister).len(),
        before,
        "a cancel closes no attempt, so it writes no line"
    );

    // the entry left the outbox, so nothing is read for it again
    advance(&pic, canister, Duration::from_secs(300));
    assert!(
        pic.get_canister_http().is_empty(),
        "the cancel is done and the outbox is empty"
    );
}

/// An upgrade in the middle of a send loses nothing: the number handed out is in the fold,
/// not in a timer or a heap cell, so the pass the upgrade cleared is armed again and the
/// cancel still happens (A9 over rule A5).
#[test]
fn an_upgrade_between_the_allocation_and_the_cancel_loses_nothing() {
    let (pic, canister, admin) = setup_with_swaps(1);
    append(&pic, canister, admin, &created_at(0)).expect("the allocation is admitted");
    upgrade(&pic, canister, admin).expect("the upgrade goes through");
    assert!(
        pic.get_canister_http().is_empty(),
        "nothing has gone out yet"
    );

    advance(&pic, canister, STRANDED_AFTER);
    assert_eq!(
        of_kind(events(&pic, canister), is_cancelled).len(),
        1,
        "the allocation was in the fold, so the upgrade could not lose it"
    );
}

/// Answers one pending outcall with a JSON-RPC error member per call, which is how a
/// provider refuses a broadcast.
fn reply_error(pic: &PocketIc, request: &CanisterHttpRequest, messages: Vec<&str>) {
    let body: Vec<Value> = messages
        .into_iter()
        .enumerate()
        .map(|(id, message)| json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32000, "message": message}}))
        .collect();
    pic.mock_canister_http_response(MockCanisterHttpResponse {
        subnet_id: request.subnet_id,
        request_id: request.request_id,
        response: CanisterHttpResponse::CanisterHttpReply(CanisterHttpReply {
            status: 200,
            headers: vec![],
            body: Value::Array(body).to_string().into_bytes(),
        }),
        additional_responses: vec![],
    });
    for _ in 0..12 {
        pic.tick();
    }
}

/// Rule A5's replacement clock runs from the first broadcast and not from the last, so a
/// rebroadcast can never postpone it. With the clock on the last broadcast, a pass every
/// rebroadcast window resets it forever and the fee is never bumped: an underpriced
/// transaction would hold its swap for as long as the chain refused to mine it.
#[test]
fn a_rebroadcast_never_postpones_the_replacement() {
    let (pic, canister, admin) = setup_with_swaps(1);
    send(&pic, canister, admin, 0).expect("the send goes through");
    advance(&pic, canister, BATCH_WINDOW);
    reply(&pic, &pic.get_canister_http()[0], vec![json!("0xabc")]);

    // a pass every ten seconds for four minutes: the rebroadcast window is thirty seconds,
    // so the bytes go out again and again, and the stuck window is two minutes
    for block in 0..24 {
        advance(&pic, canister, Duration::from_secs(10));
        answer_pending(&pic, 19_000_000 + block, &json!(null));
    }
    assert!(
        !of_kind(events(&pic, canister), is_replaced).is_empty(),
        "the transaction was out for four minutes, so it was replaced at a higher fee"
    );
}

/// A reverted receipt closes an attempt as a failure, and like a successful one it has to
/// be deep enough first. A one-block reorg that drops a reverted transaction and includes
/// the replacement that pays the user would otherwise leave the attempt closed, the outbox
/// no longer watching, and the engine free to pay a second time.
#[test]
fn a_reverted_receipt_is_no_failure_until_it_is_deep_enough() {
    let (pic, canister, admin) = setup_with_swaps(1);
    let tx_hash = send(&pic, canister, admin, 0).expect("the send goes through");
    advance(&pic, canister, BATCH_WINDOW);
    reply(&pic, &pic.get_canister_http()[0], vec![json!("0xabc")]);

    // reverted, in a block the head has not reached: two moments of the chain
    advance(&pic, canister, BATCH_WINDOW);
    answer_pending(&pic, 19_000_000, &receipt(19_000_005, false, tx_hash));
    assert!(
        of_kind(events(&pic, canister), is_failed).is_empty(),
        "nothing fails on a receipt ahead of the head"
    );

    advance(&pic, canister, BATCH_WINDOW);
    answer_pending(&pic, 19_000_005, &receipt(19_000_005, false, tx_hash));
    assert_eq!(
        of_kind(events(&pic, canister), is_failed).len(),
        1,
        "at depth it fails the attempt"
    );
}

/// A receipt decides nothing unless it is about a transaction this canister broadcast. One
/// unreplicated provider answers both the head and the receipts, so without this it alone
/// decides that an attempt confirmed, at a height it also supplies, for a transaction that
/// may not exist.
#[test]
fn a_receipt_for_a_transaction_we_never_sent_closes_nothing() {
    let (pic, canister, admin) = setup_with_swaps(1);
    send(&pic, canister, admin, 0).expect("the send goes through");
    advance(&pic, canister, BATCH_WINDOW);
    reply(&pic, &pic.get_canister_http()[0], vec![json!("0xabc")]);

    let foreign: Hash32 = [0xaa; 32];
    advance(&pic, canister, BATCH_WINDOW);
    answer_pending(&pic, 19_000_005, &receipt(19_000_005, true, foreign));
    assert!(
        of_kind(events(&pic, canister), is_confirmed).is_empty(),
        "a receipt for a foreign hash is not this attempt's receipt"
    );

    // and a forged reverted one is not a failure either
    advance(&pic, canister, BATCH_WINDOW);
    answer_pending(&pic, 19_000_006, &receipt(19_000_006, false, foreign));
    assert!(
        of_kind(events(&pic, canister), is_failed).is_empty(),
        "nor is it this attempt's failure"
    );
}

/// A provider answering "nonce too low" is saying one of this entry's own transactions is
/// already mined, because this canister is the only account that spends its nonces. The
/// entry goes to the receipt reader rather than back into the queue, where it would sit
/// forever: `check_open` reads only what has been sent. A provider that lies about it
/// self-corrects, because the entry then follows the normal receipt, rebroadcast and
/// replacement path and an honest chain refuses the replacement.
#[test]
fn a_nonce_the_provider_calls_too_low_goes_to_the_receipt_reader() {
    let (pic, canister, admin) = setup_with_swaps(1);
    send(&pic, canister, admin, 0).expect("the send goes through");
    advance(&pic, canister, BATCH_WINDOW);
    let pending = pic.get_canister_http();
    assert_eq!(methods(&pending[0]), vec!["eth_sendRawTransaction"]);
    reply_error(&pic, &pending[0], vec!["nonce too low"]);

    advance(&pic, canister, BATCH_WINDOW);
    let pending = pic.get_canister_http();
    assert_eq!(pending.len(), 1, "one chain, one batch");
    assert_eq!(
        methods(&pending[0]),
        vec!["eth_blockNumber", "eth_getTransactionReceipt"],
        "the entry is being watched, not broadcast again"
    );
}

/// The cap on an outcall's answer follows the batch that outcall is for. A fixed cap is a
/// cliff: one batch above it is rejected by the system, every later pass builds the same
/// oversized batch, and nothing on that chain ever broadcasts or ever closes again.
#[test]
fn the_outcall_caps_follow_the_batch_they_are_for() {
    let (pic, canister, admin) = setup_with_swaps(2);
    let first = send(&pic, canister, admin, 0).expect("the first send goes through");
    send(&pic, canister, admin, 1).expect("the second send goes through");

    advance(&pic, canister, BATCH_WINDOW);
    let pending = pic.get_canister_http();
    assert_eq!(pending.len(), 1, "one chain, one batch");
    assert_eq!(methods(&pending[0]).len(), 2, "two broadcasts in it");
    let two_sends = pending[0]
        .max_response_bytes
        .expect("every outcall reserves a cap");
    reply(&pic, &pending[0], vec![json!("0xabc"), json!("0xdef")]);

    // the receipt batch is the head plus one receipt per hash, and its cap is sized for a
    // real receipt: a vault execute or a CCTP depositForBurn carries several kilobytes of
    // logs, not the few hundred bytes a transaction hash is
    advance(&pic, canister, BATCH_WINDOW);
    let pending = pic.get_canister_http();
    let two_receipts = pending
        .iter()
        .find(|request| methods(request).contains(&"eth_getTransactionReceipt".to_string()))
        .expect("the receipts are asked for")
        .max_response_bytes
        .expect("every outcall reserves a cap");
    assert!(
        two_receipts > two_sends * 4,
        "a receipt is orders of magnitude bigger than a transaction hash: {two_receipts} \
         against {two_sends}"
    );

    // the first transaction lands, so the next pass reads receipts for one entry, and the
    // cap shrinks with it: it is the batch's own and not a constant the batch fits inside
    answer_pending(&pic, 19_000_005, &receipt(19_000_005, true, first));
    assert_eq!(
        of_kind(events(&pic, canister), is_confirmed).len(),
        1,
        "one of the two closed"
    );
    advance(&pic, canister, BATCH_WINDOW);
    let pending = pic.get_canister_http();
    let one_receipt = pending
        .iter()
        .find(|request| methods(request).contains(&"eth_getTransactionReceipt".to_string()))
        .expect("the entry still open is still watched")
        .max_response_bytes
        .expect("every outcall reserves a cap");
    assert!(
        one_receipt < two_receipts && one_receipt * 2 > two_receipts,
        "one receipt reserves about half of what two reserve: {one_receipt} against \
         {two_receipts}"
    );
}

/// The halt switch is the emergency stop of a canister that custodies funds, so nothing new
/// reaches a chain while it is on. A transaction signed seconds before an operator halts is
/// still in the queue, and it waits there: the nonce is not abandoned, and lifting the halt
/// sends it.
#[test]
fn a_halted_canister_broadcasts_nothing_new() {
    let (pic, canister, admin) = setup_with_swaps(1);
    let signed = {
        send(&pic, canister, admin, 0).expect("the send goes through");
        tx_signed(&events(&pic, canister))
    };
    set_halted(&pic, canister, admin, true).expect("a controller may halt");

    advance(&pic, canister, BATCH_WINDOW);
    assert!(
        pic.get_canister_http().is_empty(),
        "a halted canister hands nothing to a provider"
    );
    advance(&pic, canister, Duration::from_secs(300));
    assert!(
        pic.get_canister_http().is_empty(),
        "and it does not start later either"
    );

    set_halted(&pic, canister, admin, false).expect("a controller may resume");
    advance(&pic, canister, BATCH_WINDOW);
    let pending = pic.get_canister_http();
    assert_eq!(
        pending.len(),
        1,
        "the transaction was waiting, not abandoned"
    );
    assert_eq!(
        params(&pending[0], 0),
        json!([format!("0x{}", hex::encode(&signed[0]))]),
        "and it is the transaction the log recorded"
    );
}

/// The cancel's tx hash, as the log recorded it.
fn cancel_hash(pic: &PocketIc, canister: Principal) -> Hash32 {
    let EventType::TxCancelled { tx_hash, .. } = events(pic, canister)
        .into_iter()
        .map(|event| event.payload)
        .find(is_cancelled)
        .expect("the cancel is in the log")
    else {
        unreachable!()
    };
    tx_hash
}

/// The rounds a threshold signature takes, with nothing else moving.
fn settle(pic: &PocketIc) {
    for _ in 0..12 {
        pic.tick();
    }
}

/// An allocation is stranded on a clock of its own and never on the batch window. The
/// signature is awaited across consensus rounds on the signing subnet, a round trip that
/// is several windows long, so a cancel fired after one window races the very signature it
/// stands in for. Here the allocation is left alone through window after window and a
/// whole minute, and is cancelled by one pass once it is older than a signing round trip
/// could possibly be.
#[test]
fn an_allocation_is_stranded_only_once_a_signing_round_trip_could_not_still_be_out() {
    let (pic, canister, admin) = setup_with_swaps(1);
    append(&pic, canister, admin, &created_at(0)).expect("the allocation is admitted");

    for window in 1..=5 {
        advance(&pic, canister, BATCH_WINDOW);
        assert!(
            of_kind(events(&pic, canister), is_cancelled).is_empty(),
            "window {window}: an allocation younger than a signing round trip is not stranded"
        );
        assert!(
            pic.get_canister_http().is_empty(),
            "window {window}: and nothing is broadcast for it"
        );
    }
    advance(&pic, canister, Duration::from_secs(60));
    assert!(
        of_kind(events(&pic, canister), is_cancelled).is_empty(),
        "a minute is still inside the bound"
    );

    advance(&pic, canister, STRANDED_AFTER);
    let cancelled = tx_cancelled(&events(&pic, canister));
    assert_eq!(cancelled.len(), 1, "past the bound, one pass cancels it");
    assert_eq!((cancelled[0].0, cancelled[0].1), (BASE, 0));
}

/// The race the stranding clock cannot fully close, closed at the fold: a cancel spent the
/// number while the signature was on its way, and the signed record that comes back late
/// is refused. Nothing of it reaches the outbox, so the cancel's entry at that nonce stays
/// whole: its bytes are what goes out, its hash is what the receipt reader asks for, the
/// log still folds, and the swap sends again at the next number. The real interleaving
/// cannot be forced here, because both signatures ride the same signing subnet in the order
/// they were asked for, so the late record arrives through the test door.
#[test]
fn a_cancelled_nonce_refuses_the_signature_that_comes_back_late() {
    let (pic, canister, admin) = setup_with_swaps(1);
    append(&pic, canister, admin, &created_at(0)).expect("the allocation is admitted");
    advance(&pic, canister, STRANDED_AFTER);
    let cancelled = tx_cancelled(&events(&pic, canister));
    assert_eq!(cancelled.len(), 1, "the number was spent by a cancel");
    let cancel_raw = cancelled[0].2.clone();
    let cancel_hash = cancel_hash(&pic, canister);

    let late = EventType::TxSigned {
        quote_hash: swap_id(0),
        attempt: 1,
        chain_id: BASE,
        tx_hash: [0x77; 32],
        raw_tx: vec![0x02, 0xf8, 0x6b],
    };
    assert_eq!(
        append(&pic, canister, admin, &late),
        Err(TestAppendError::Append(AppendError::Transition(
            TransitionError::NoUnsignedNonce {
                quote_hash: swap_id(0),
                chain_id: BASE,
            }
        ))),
        "the number this signature was made for is gone"
    );
    assert!(
        verify_replay(&pic, canister, Principal::anonymous()),
        "the log still folds to the live fold"
    );

    // the outbox holds exactly the cancel: its bytes are what is broadcast
    let pending = pic.get_canister_http();
    let sends: Vec<&CanisterHttpRequest> = pending
        .iter()
        .filter(|request| methods(request).contains(&"eth_sendRawTransaction".to_string()))
        .collect();
    assert_eq!(sends.len(), 1, "one entry at the nonce, the cancel's");
    assert_eq!(
        params(sends[0], 0),
        json!([format!("0x{}", hex::encode(&cancel_raw))]),
        "and the bytes are the cancel's"
    );
    reply(&pic, sends[0], vec![json!("0xabc")]);

    // and its hash is the only one the receipt reader looks up
    let pending = pic.get_canister_http();
    let receipts: Vec<&CanisterHttpRequest> = pending
        .iter()
        .filter(|request| methods(request).contains(&"eth_getTransactionReceipt".to_string()))
        .collect();
    assert_eq!(receipts.len(), 1, "one receipt batch for the one entry");
    assert_eq!(
        methods(receipts[0]),
        vec!["eth_blockNumber", "eth_getTransactionReceipt"],
        "the head and one receipt: nothing of the refused record is in the outbox"
    );
    assert_eq!(
        params(receipts[0], 1),
        json!([format!("0x{}", hex::encode(cancel_hash))]),
        "and it is the cancel's"
    );
    answer_pending(&pic, 19_000_000, &json!(null));

    // the swap is free to send at the next number
    send(&pic, canister, admin, 0).expect("the swap may send again");
    assert_eq!(
        tx_created(&events(&pic, canister)),
        vec![(BASE, 0), (BASE, 1)],
        "the allocator moved on past the cancelled number"
    );
    assert!(verify_replay(&pic, canister, Principal::anonymous()));
}

/// Each cancel of a pass reads the clock for itself. The cancels before it each awaited a
/// signature, so the instant the pass started is stale by a round trip per cancel: priced
/// on it, the second cancel would take a reading that aged out while the first was
/// signing. Here the reading ages out during the first cancel's signature, the second
/// cancel refuses to price on it, and the watcher's next push lets the next pass cancel it.
#[test]
fn each_cancel_of_a_pass_reads_the_clock_for_itself() {
    let (pic, canister, admin) = setup_with_swaps(2);
    append(&pic, canister, admin, &created_for(0, 0)).expect("the first allocation");
    append(&pic, canister, admin, &created_for(1, 1)).expect("the second allocation");

    // both are stranded: the round the pass starts in prices the first cancel and asks
    // for its signature, which comes back in a later round
    pic.advance_time(STRANDED_AFTER);
    push_reading(&pic, canister);
    pic.tick();
    assert!(
        of_kind(events(&pic, canister), is_cancelled).is_empty(),
        "the first cancel is still waiting for its signature"
    );

    // the reading ages past `chain_data_max_age` while that signature is on its way, and
    // no watcher pushes another
    pic.advance_time(Duration::from_secs(20));
    settle(&pic);
    settle(&pic);
    let cancelled = tx_cancelled(&events(&pic, canister));
    assert_eq!(
        cancelled
            .iter()
            .map(|(_, nonce, _)| *nonce)
            .collect::<Vec<_>>(),
        vec![0],
        "the first cancel was priced before the reading aged out; the second read the clock \
         for itself and refused the reading, rather than pricing on the pass's instant"
    );

    // the pass broadcasts the one cancel it signed and reads its receipt, then re-arms for
    // the number it could not price; the next push is fresh at that pass's instant, so
    // the second number is cancelled by it
    answer_pending(&pic, 19_000_000, &json!(null));
    answer_pending(&pic, 19_000_000, &json!(null));
    advance(&pic, canister, BATCH_WINDOW);
    settle(&pic);
    let cancelled = tx_cancelled(&events(&pic, canister));
    assert_eq!(
        cancelled
            .iter()
            .map(|(_, nonce, _)| *nonce)
            .collect::<Vec<_>>(),
        vec![0, 1],
        "nothing was abandoned"
    );
    assert!(verify_replay(&pic, canister, Principal::anonymous()));
}

/// The system refusing an answer bigger than the cap the outcall reserved, which is how an
/// oversized reply reaches the canister on the network: not as bytes, but as a rejection of
/// the whole call. pocket-ic delivers a mocked body whatever its size, so the refusal is
/// mocked the way the replica makes it.
fn reject_oversized(pic: &PocketIc, request: &CanisterHttpRequest) {
    let cap = request
        .max_response_bytes
        .expect("every outcall reserves a cap");
    pic.mock_canister_http_response(MockCanisterHttpResponse {
        subnet_id: request.subnet_id,
        request_id: request.request_id,
        response: CanisterHttpResponse::CanisterHttpReject(CanisterHttpReject {
            reject_code: 1,
            message: format!("Http body exceeds size limit of {cap} bytes."),
        }),
        additional_responses: vec![],
    });
    for _ in 0..12 {
        pic.tick();
    }
}

/// A chunk whose answer does not come back whole is read again one entry at a time, so one
/// receipt too big for the reply stalls nothing but its own entry: the other two close on
/// their own answers, and the one that cannot be read is not left silent, its bytes go out
/// again on the rebroadcast cadence like a transaction no receipt came back for. Without
/// the re-read every pass builds the same chunk, the same answer is refused, and nothing on
/// the chain ever closes again.
#[test]
fn a_chunk_whose_answer_does_not_come_back_whole_is_read_one_entry_at_a_time() {
    let (pic, canister, admin) = setup_with_swaps(3);
    let hashes: Vec<Hash32> = (0..3)
        .map(|swap| send(&pic, canister, admin, swap).expect("the send goes through"))
        .collect();
    let signed = tx_signed(&events(&pic, canister));
    advance(&pic, canister, BATCH_WINDOW);
    let pending = pic.get_canister_http();
    assert_eq!(pending.len(), 1, "one batch of three broadcasts");
    assert_eq!(methods(&pending[0]).len(), 3);
    reply(
        &pic,
        &pending[0],
        vec![json!("0xa"), json!("0xb"), json!("0xc")],
    );

    // the three receipts are asked for in one chunk, and one of them is bigger than the
    // whole answer may be, so the system refuses the answer
    let pending = pic.get_canister_http();
    assert_eq!(pending.len(), 1, "one chunk for the three");
    assert_eq!(methods(&pending[0]).len(), 4, "the head and three receipts");
    reject_oversized(&pic, &pending[0]);
    assert!(
        of_kind(events(&pic, canister), is_confirmed).is_empty(),
        "an answer that did not come back whole decides nothing"
    );

    // so each entry is read on its own, and the first two close on their own answers
    let deep = 19_000_010;
    let head = json!(format!("0x{deep:x}"));
    for (entry, hash) in hashes.iter().take(2).enumerate() {
        let pending = pic.get_canister_http();
        assert_eq!(pending.len(), 1, "entry {entry} is read alone");
        assert_eq!(
            methods(&pending[0]),
            vec!["eth_blockNumber", "eth_getTransactionReceipt"],
            "entry {entry}: the head and its one receipt"
        );
        assert_eq!(
            params(&pending[0], 1),
            json!([format!("0x{}", hex::encode(hash))]),
            "entry {entry}: its own hash"
        );
        reply(
            &pic,
            &pending[0],
            vec![head.clone(), receipt(deep, true, *hash)],
        );
    }
    assert_eq!(
        of_kind(events(&pic, canister), is_confirmed).len(),
        2,
        "the two that could be read closed"
    );

    // the third's answer is the oversized one, refused on its own too, which decides
    // nothing about it
    let pending = pic.get_canister_http();
    assert_eq!(pending.len(), 1, "the third is read alone");
    assert_eq!(
        params(&pending[0], 1),
        json!([format!("0x{}", hex::encode(hashes[2]))])
    );
    reject_oversized(&pic, &pending[0]);
    assert_eq!(of_kind(events(&pic, canister), is_confirmed).len(), 2);
    assert!(
        of_kind(events(&pic, canister), is_failed).is_empty(),
        "a receipt that cannot be read is no failure"
    );

    // and it is not left silent: past the rebroadcast window its receipt is asked for
    // again, and when that cannot be read either its bytes go out again
    advance(&pic, canister, Duration::from_secs(30));
    let pending = pic.get_canister_http();
    assert_eq!(pending.len(), 1, "the third is still watched");
    assert_eq!(
        params(&pending[0], 1),
        json!([format!("0x{}", hex::encode(hashes[2]))])
    );
    reject_oversized(&pic, &pending[0]);
    advance(&pic, canister, BATCH_WINDOW);
    let pending = pic.get_canister_http();
    let sends: Vec<&CanisterHttpRequest> = pending
        .iter()
        .filter(|request| methods(request).contains(&"eth_sendRawTransaction".to_string()))
        .collect();
    assert_eq!(sends.len(), 1, "the unread entry is pushed again");
    assert_eq!(
        params(sends[0], 0),
        json!([format!("0x{}", hex::encode(&signed[2]))]),
        "with the bytes the log recorded for it"
    );
}

/// A halted canister rests. The pass it had armed runs once, finds work it may not do, and
/// does not put itself on the next window; the halt switch is what wakes it, so lifting
/// the halt arms the pass and it does what it could not. Without that a halted canister
/// runs an empty pass every window for as long as the halt lasts.
#[test]
fn a_halted_canister_rests_until_the_halt_lifts() {
    let (pic, canister, admin) = setup_with_swaps(1);
    append(&pic, canister, admin, &created_at(0)).expect("the allocation is admitted");
    set_halted(&pic, canister, admin, true).expect("a controller may halt");

    // the number is stranded and the armed pass finds it, but a halted canister may not
    // cancel it
    advance(&pic, canister, STRANDED_AFTER);
    assert!(
        of_kind(events(&pic, canister), is_cancelled).is_empty(),
        "a halted canister signs no cancel"
    );
    assert!(
        !test_outbox_armed(&pic, canister, Principal::anonymous()),
        "and the pass does not re-arm for work it may not do"
    );
    for window in 1..=3 {
        advance(&pic, canister, BATCH_WINDOW);
        assert!(
            !test_outbox_armed(&pic, canister, Principal::anonymous()),
            "window {window}: still resting"
        );
        assert!(
            pic.get_canister_http().is_empty(),
            "window {window}: and nothing goes out"
        );
    }

    set_halted(&pic, canister, admin, false).expect("a controller may resume");
    assert!(
        test_outbox_armed(&pic, canister, Principal::anonymous()),
        "lifting the halt wakes the pass"
    );
    advance(&pic, canister, BATCH_WINDOW);
    settle(&pic);
    assert_eq!(
        tx_cancelled(&events(&pic, canister)).len(),
        1,
        "and it does what it could not while halted"
    );
}
