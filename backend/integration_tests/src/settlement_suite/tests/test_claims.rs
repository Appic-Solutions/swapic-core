//! The entry doors from outside: the claim that creates a swap from a deposit the chain
//! holds, the gasless pull that makes such a deposit, and the attestation inbox.
//!
//! The claim's outcalls are mocked here the way the outbox tests mock theirs: the test
//! submits the call, reads the pending requests, and answers them from a mocked provider
//! ([`Provider`]): the provider's head, asked alone first, then the vault's logs window by
//! window, and the time of the block a deposit landed in when a claim comes late.

use crate::client::settlement::{
    claim_swap, derive_evm_address, event_count, events_page, get_pending, get_swap, paused_swaps,
    push_attestation, push_chain_data, push_eco_intent, register_quote, set_halted, set_sanctioned,
    start_gasless_pull, verify_replay,
};
use crate::settlement_suite::init::{empty_canister, install, quoter, watcher};
use candid::{encode_one, Nat, Principal};
use pocket_ic::common::rest::{
    CanisterHttpReject, CanisterHttpReply, CanisterHttpRequest, CanisterHttpResponse,
    MockCanisterHttpResponse, RawMessageId,
};
use pocket_ic::{PocketIc, PocketIcBuilder, Time};
use serde_json::{json, Value};
use settlement_api::types::chain_data::ChainData;
use settlement_api::types::config::Config;
use settlement_api::types::entry::{
    ClaimError, DepositError, Permit2Sig, PermitError, PermitMismatch, PermitSig, PullError,
    PullRequest, PushAttestationError,
};
use settlement_api::types::errors::GuardError;
use settlement_api::types::events::{Event, EventType, Hash32, TxPurpose};
use settlement_api::types::init::InitArg;
use settlement_api::types::quote::Quote;
use settlement_api::types::swap::{PausedSwapsPage, SwapStatus};
use std::collections::BTreeMap;
use std::time::Duration;
use types::abi::deposited_topic;
use types::{ChainId, GasMode, Rail, TokenAmount, UnixSeconds};

const BASE: u64 = 8453;
const VAULT: &str = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913";
const USDC: &str = "0xaf88d065e77c8cC2239327C5EDb3A432268e5831";
/// The user's wallet: the fixture quote's refund address, which on an EVM source chain is
/// the paying wallet, so it is the wallet every deposit of the user's comes from.
const USER: &str = "0x7551A66653f9a20979ed81835a0b7008EC83401b";
/// A wallet that is not the quote's: whatever it deposits under the quote's hash is not
/// the quote's deposit.
const STRANGER: &str = "0x1111111111111111111111111111111111111111";
/// Where the fixture quote pays its user: an address, as a quote must name one.
const DST: &str = "0x4444444444444444444444444444444444444444";
const HEAD: u64 = 19_000_000;
const AMOUNT: u32 = 25_000_000;
/// The second the fixture quote expires at. The clock moves to the quote rather than the
/// quote to the clock, because a quote is claimed only after the quoter registered it and
/// a registration is refused for a quote expiring more than a day out.
const EXPIRES_AT: u64 = 1_800_000_000;

/// A quote for a deposit on Base: the source token is the token contract, as a quote on
/// an EVM chain names it.
fn quote(nonce: u64) -> types::Quote {
    types::Quote {
        version: 1,
        src_chain: ChainId::BASE,
        src_token: USDC.parse().unwrap(),
        amount_in: TokenAmount::from(AMOUNT),
        dst_chain: ChainId::ARBITRUM,
        dst_token: USDC.parse().unwrap(),
        expected_out: TokenAmount::from(24_990_000_u32),
        min_out: TokenAmount::from(24_900_000_u32),
        // a payout and a refund have to be payable, so the quote names addresses, not text;
        // the refund address is the paying wallet, the one the user's deposits come from
        dst_address: DST.parse().unwrap(),
        refund_address: Some(USER.parse().unwrap()),
        auto_refund: true,
        gas_mode: GasMode::Legacy,
        rail: Rail::CctpV2Fast,
        expires_at: UnixSeconds::new(EXPIRES_AT),
        nonce,
    }
}

fn wire(quote: &types::Quote) -> Quote {
    Quote::from(quote.clone())
}

fn swap_id(quote: &types::Quote) -> Hash32 {
    quote.hash().expect("a valid quote has an id").into_bytes()
}

const ARBITRUM: u64 = 42161;

/// A provider and a vault on Base, and the rails' token on both chains of the fixture
/// quote, which is what a claim pins the quote's tokens to.
fn config() -> Config {
    Config {
        rpc_urls: BTreeMap::from([(BASE, "https://base-mainnet.example/v2/key".to_string())]),
        vault_addresses: BTreeMap::from([(BASE, VAULT.to_string())]),
        usdc_addresses: BTreeMap::from([(BASE, USDC.to_string()), (ARBITRUM, USDC.to_string())]),
        ..Config::default()
    }
}

/// A canister with a provider and a vault on Base and a fresh reading, on a network with
/// no signing key: a claim reads, it does not sign.
fn setup() -> (PocketIc, Principal, Principal) {
    let (pic, canister, admin) = empty_canister();
    let arg = InitArg {
        config: config(),
        quoter: quoter(),
        watcher: watcher(),
    };
    install(&pic, canister, admin, &arg).expect("the arg installs");
    set_the_clock_inside_the_fixtures_window(&pic);
    push_reading(&pic, canister);
    (pic, canister, admin)
}

/// Ten minutes before the fixture quote expires: inside the window the store registers a
/// quote in, and inside the window a claim is admitted in.
fn set_the_clock_inside_the_fixtures_window(pic: &PocketIc) {
    pic.set_time(Time::from_nanos_since_unix_epoch(
        (EXPIRES_AT - 600) * 1_000_000_000,
    ));
}

/// The same on a network holding the test threshold keys, for the pull, which signs.
fn setup_with_keys() -> (PocketIc, Principal, Principal) {
    let pic = PocketIcBuilder::new()
        .with_ii_subnet()
        .with_application_subnet()
        .build();
    let admin = Principal::from_slice(&[1; 29]);
    let subnet = pic.topology().get_app_subnets()[0];
    let canister = pic.create_canister_on_subnet(Some(admin), None, subnet);
    pic.add_cycles(canister, 1_000_000_000_000_000);
    let arg = InitArg {
        config: config(),
        quoter: quoter(),
        watcher: watcher(),
    };
    install(&pic, canister, admin, &arg).expect("the arg installs");
    derive_evm_address(&pic, canister, admin).expect("the test key derives an address");
    set_the_clock_inside_the_fixtures_window(&pic);
    push_reading(&pic, canister);
    (pic, canister, admin)
}

fn push_reading(pic: &PocketIc, canister: Principal) {
    push_chain_data(
        pic,
        canister,
        watcher(),
        BASE,
        &ChainData {
            block: HEAD,
            base_fee_wei_per_gas: Nat::from(1_000_000_000_u64),
            priority_fee_wei_per_gas: Nat::from(100_000_000_u64),
        },
    )
    .expect("the watcher may push");
}

fn events(pic: &PocketIc, canister: Principal) -> Vec<Event> {
    events_page(pic, canister, Principal::anonymous(), 0, 500)
}

fn count(pic: &PocketIc, canister: Principal) -> u64 {
    event_count(pic, canister, Principal::anonymous())
}

/// The quoter registers the quote, which is what a claim needs: a swap's economics are
/// the quoter's, so only a quote the store holds becomes one. Answers the swap id.
fn registered(pic: &PocketIc, canister: Principal, quote: &types::Quote) -> Hash32 {
    register_quote(pic, canister, quoter(), &wire(quote)).expect("the quoter registers the quote")
}

/// Registers the quote the way the quoter would, submits a claim for it, and gives the
/// canister the rounds it needs to reach its outcall. A test about an unregistered quote
/// calls the door itself.
fn submit_claim(
    pic: &PocketIc,
    canister: Principal,
    who: Principal,
    quote: &types::Quote,
) -> RawMessageId {
    registered(pic, canister, quote);
    submit_claim_unregistered(pic, canister, who, &wire(quote))
}

/// Submits a claim without registering anything first.
fn submit_claim_unregistered(
    pic: &PocketIc,
    canister: Principal,
    who: Principal,
    quote: &Quote,
) -> RawMessageId {
    let id = pic
        .submit_call(canister, who, "claim_swap", encode_one(quote).unwrap())
        .expect("the door accepts the call");
    pic.tick();
    pic.tick();
    id
}

fn await_claim(pic: &PocketIc, call: RawMessageId) -> Result<Hash32, ClaimError> {
    candid::decode_one(&pic.await_call(call).expect("the claim returns")).unwrap()
}

/// The methods one pending outcall asks for, in order.
fn methods(request: &CanisterHttpRequest) -> Vec<String> {
    let body: Value = serde_json::from_slice(&request.body).expect("the body is json");
    body.as_array()
        .expect("a batch is an array")
        .iter()
        .map(|call| call["method"].as_str().expect("a method").to_string())
        .collect()
}

fn params(request: &CanisterHttpRequest, n: usize) -> Value {
    let body: Value = serde_json::from_slice(&request.body).expect("the body is json");
    body[n]["params"].clone()
}

fn word_of(address: &str) -> String {
    let address: types::EvmAddress = address.parse().unwrap();
    format!("0x{}", hex::encode(address.to_word()))
}

/// The vault's `Deposited` log for `quote_hash`, as a provider answers it.
fn deposit_log(quote_hash: Hash32, block: u64, token: &str, from: &str, amount: u64) -> Value {
    logged_deposit(quote_hash, block, token, from, amount, [0x77; 32])
}

/// A `Deposited` log for `quote_hash` from somebody else: a griefer's dust at `block`,
/// in its own transaction.
fn dust_log(quote_hash: Hash32, block: u64, amount: u64) -> Value {
    logged_deposit(quote_hash, block, USDC, STRANGER, amount, [0x66; 32])
}

fn logged_deposit(
    quote_hash: Hash32,
    block: u64,
    token: &str,
    from: &str,
    amount: u64,
    tx_hash: Hash32,
) -> Value {
    json!({
        "address": VAULT.to_ascii_lowercase(),
        "topics": [
            format!("0x{}", hex::encode(deposited_topic())),
            format!("0x{}", hex::encode(quote_hash)),
            word_of(token),
            word_of(from),
        ],
        "data": format!("0x{amount:064x}"),
        "blockNumber": format!("0x{block:x}"),
        "blockHash": format!("0x{}", hex::encode([0x42; 32])),
        "transactionHash": format!("0x{}", hex::encode(tx_hash)),
        "logIndex": "0x2",
        "removed": false,
    })
}

/// How a provider answers a range of blocks above its own head, as the review of fix wave
/// 4 measured real ones (M1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    /// Geth, and the providers built on it: a range that starts above the head, or names
    /// a block above it by number, is refused with an error.
    Geth,
    /// A provider that answers any range with the logs it holds, and none above its head.
    Permissive,
}

/// A mocked provider for Base: what its `eth_blockNumber` says, the highest block its logs
/// reach, the vault's logs, the time each block was made, and every read the canister made.
struct Provider {
    head: u64,
    /// The highest block a log is served from: the head for a real node, and past it for
    /// the fixtures that hand a log over before the head reaches it.
    tip: u64,
    shape: Shape,
    logs: Vec<Value>,
    /// The block a hash names, and the second it was made at.
    blocks: BTreeMap<String, (u64, u64)>,
    /// The quote every log read must be for, when the test names one.
    quote_hash: Option<Hash32>,
    /// Every `eth_getLogs` asked: the first block, and the last (`None` for the head).
    log_reads: Vec<(u64, Option<u64>)>,
    /// Every block hash asked for.
    block_reads: Vec<String>,
    /// How many outcalls were answered.
    outcalls: usize,
    /// The most calls one batch may carry: a longer batch is refused whole, the way a
    /// provider with a batch limit refuses it.
    max_batch: Option<usize>,
    /// How many batches were refused for their length.
    refused_batches: usize,
    /// A block whose logs the provider cannot serve: any range covering it is refused, the
    /// way a provider refuses a range whose answer would be too large.
    unservable: Option<u64>,
    /// For every outcall answered, the cap the canister reserved and the length of the body
    /// the provider had for it.
    sizes: Vec<(Option<u64>, usize)>,
    /// Whether an answer longer than the cap its outcall reserved is refused the way the
    /// replica refuses it (pocket-ic hands a mocked body back whatever its size).
    enforce_cap: bool,
    /// The token word every `eth_getLogs` filtered on (`topics[2]`), null where it named
    /// none.
    token_topics: Vec<Value>,
    /// The payer word every `eth_getLogs` filtered on (`topics[3]`), null where it named
    /// none.
    payer_topics: Vec<Value>,
    /// Whether the provider ignores the payer a filter names and answers every payer's
    /// logs, as a node that does not listen would. A real node filters on every topic.
    deaf_to_the_payer: bool,
    /// For every outcall answered, how many log reads had been asked by the end of it, so
    /// a test can tell which log reads came after a given outcall.
    answered: Vec<usize>,
}

/// A hex quantity as a number.
fn hex_u64(text: &str) -> u64 {
    u64::from_str_radix(text.trim_start_matches("0x"), 16).expect("a hex quantity")
}

impl Provider {
    /// A node at `head` holding `logs`, which serves nothing above its head and answers
    /// any range.
    fn at(head: u64, logs: &[Value]) -> Self {
        Self {
            head,
            tip: head,
            shape: Shape::Permissive,
            logs: logs.to_vec(),
            blocks: BTreeMap::new(),
            quote_hash: None,
            log_reads: Vec::new(),
            block_reads: Vec::new(),
            outcalls: 0,
            max_batch: None,
            refused_batches: 0,
            unservable: None,
            sizes: Vec::new(),
            enforce_cap: false,
            token_topics: Vec::new(),
            payer_topics: Vec::new(),
            deaf_to_the_payer: false,
            answered: Vec::new(),
        }
    }

    /// A provider that ignores the payer a filter names and hands over every payer's logs:
    /// the one way a stranger's dust under the quote's hash still reaches the read, which
    /// the canister then refuses log by log.
    fn deaf_to_the_payer(self) -> Self {
        Self {
            deaf_to_the_payer: true,
            ..self
        }
    }

    /// A provider behind the replica's size rule: an answer longer than the cap its outcall
    /// reserved is refused, as the replica refuses it, and not handed over.
    fn enforcing_caps(self) -> Self {
        Self {
            enforce_cap: true,
            ..self
        }
    }

    /// The outcalls whose answer was longer than the cap they reserved, as (cap, length).
    fn oversized(&self) -> Vec<(u64, usize)> {
        self.sizes
            .iter()
            .filter_map(|(cap, len)| cap.filter(|cap| *len as u64 > *cap).map(|cap| (cap, *len)))
            .collect()
    }

    /// A provider that refuses any range covering `block`.
    fn refusing_block(self, block: u64) -> Self {
        Self {
            unservable: Some(block),
            ..self
        }
    }

    /// A provider that refuses any batch of more than `calls` calls.
    fn batches_of_at_most(self, calls: usize) -> Self {
        Self {
            max_batch: Some(calls),
            ..self
        }
    }

    fn shaped(self, shape: Shape) -> Self {
        Self { shape, ..self }
    }

    fn for_quote(self, quote_hash: Hash32) -> Self {
        Self {
            quote_hash: Some(quote_hash),
            ..self
        }
    }

    /// The block `hash` names was made at `timestamp`.
    fn block(mut self, hash: Hash32, number: u64, timestamp: u64) -> Self {
        self.blocks
            .insert(format!("0x{}", hex::encode(hash)), (number, timestamp));
        self
    }

    /// The answer to one call, or the message of the error it answers with.
    fn call(&mut self, method: &str, params: &Value) -> Result<Value, String> {
        match method {
            "eth_blockNumber" => Ok(json!(format!("0x{:x}", self.head))),
            "eth_getLogs" => {
                let filter = &params[0];
                assert_eq!(filter["address"], json!(VAULT.to_ascii_lowercase()));
                assert_eq!(
                    filter["topics"][0],
                    json!(format!("0x{}", hex::encode(deposited_topic())))
                );
                if let Some(quote_hash) = self.quote_hash {
                    assert_eq!(
                        filter["topics"][1],
                        json!(format!("0x{}", hex::encode(quote_hash))),
                        "the read is for this quote"
                    );
                }
                let from = hex_u64(filter["fromBlock"].as_str().expect("a fromBlock"));
                let to = match filter["toBlock"].as_str().expect("a toBlock") {
                    "latest" => None,
                    text => Some(hex_u64(text)),
                };
                self.log_reads.push((from, to));
                let beyond = from > self.head || to.is_some_and(|to| to > self.head);
                if self.shape == Shape::Geth && beyond {
                    return Err("block range extends beyond current head block".to_string());
                }
                let covers = |block: u64| (from..=to.unwrap_or(self.tip)).contains(&block);
                if self.unservable.is_some_and(covers) {
                    return Err("query returned more than 10000 results".to_string());
                }
                let last = to.unwrap_or(self.tip).min(self.tip);
                let wanted = &filter["topics"][1];
                // a node filters on every topic the filter names, the token and the payer
                // among them
                let token = filter["topics"].get(2).cloned().unwrap_or(Value::Null);
                self.token_topics.push(token.clone());
                let payer = filter["topics"].get(3).cloned().unwrap_or(Value::Null);
                self.payer_topics.push(payer.clone());
                let payer = if self.deaf_to_the_payer {
                    Value::Null
                } else {
                    payer
                };
                let lower = |word: &Value| word.as_str().map(str::to_ascii_lowercase);
                Ok(json!(self
                    .logs
                    .iter()
                    .filter(|log| log["topics"][1] == *wanted)
                    .filter(|log| token.is_null() || lower(&log["topics"][2]) == lower(&token))
                    .filter(|log| payer.is_null() || lower(&log["topics"][3]) == lower(&payer))
                    .filter(|log| {
                        let block = hex_u64(log["blockNumber"].as_str().expect("a block"));
                        (from..=last).contains(&block)
                    })
                    .cloned()
                    .collect::<Vec<Value>>()))
            }
            "eth_getBlockByHash" => {
                let hash = params[0].as_str().expect("a block hash").to_string();
                assert_eq!(params[1], json!(false), "the header, not the bodies");
                self.block_reads.push(hash.clone());
                Ok(match self.blocks.get(&hash) {
                    Some((number, timestamp)) => json!({
                        "hash": hash,
                        "number": format!("0x{number:x}"),
                        "timestamp": format!("0x{timestamp:x}"),
                        "transactions": [],
                    }),
                    None => Value::Null,
                })
            }
            other => panic!("the canister asked for {other}, which the provider does not answer"),
        }
    }

    /// Answers one pending outcall, each call in its batch for itself, or refuses the batch
    /// whole with one error object when it is longer than the provider takes.
    fn reply(&mut self, pic: &PocketIc, request: &CanisterHttpRequest) {
        let body: Value = serde_json::from_slice(&request.body).expect("the body is json");
        let calls = body.as_array().expect("a batch is an array");
        if self.max_batch.is_some_and(|most| calls.len() > most) {
            self.refused_batches += 1;
            let refusal = json!({
                "jsonrpc": "2.0",
                "id": null,
                "error": {"code": -32600, "message": "batch too large"},
            });
            self.mock(pic, request, refusal);
            return;
        }
        let replies: Vec<Value> = calls
            .iter()
            .map(|call| {
                let id = call["id"].clone();
                match self.call(call["method"].as_str().expect("a method"), &call["params"]) {
                    Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                    Err(message) => json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": {"code": -32000, "message": message},
                    }),
                }
            })
            .collect();
        self.mock(pic, request, Value::Array(replies));
    }

    /// Hands `body` back as the answer to `request`, and gives the canister the rounds it
    /// needs to act on it.
    fn mock(&mut self, pic: &PocketIc, request: &CanisterHttpRequest, body: Value) {
        let body = body.to_string();
        self.sizes.push((request.max_response_bytes, body.len()));
        self.answered.push(self.log_reads.len());
        let cap = request
            .max_response_bytes
            .expect("every outcall reserves a cap");
        if self.enforce_cap && body.len() as u64 > cap {
            // as the replica refuses it, and as `test_outbox`'s `reject_oversized` mocks it
            pic.mock_canister_http_response(MockCanisterHttpResponse {
                subnet_id: request.subnet_id,
                request_id: request.request_id,
                response: CanisterHttpResponse::CanisterHttpReject(CanisterHttpReject {
                    reject_code: 1,
                    message: format!("Http body exceeds size limit of {cap} bytes."),
                }),
                additional_responses: vec![],
            });
            self.outcalls += 1;
            for _ in 0..4 {
                pic.tick();
            }
            return;
        }
        pic.mock_canister_http_response(MockCanisterHttpResponse {
            subnet_id: request.subnet_id,
            request_id: request.request_id,
            response: CanisterHttpResponse::CanisterHttpReply(CanisterHttpReply {
                status: 200,
                headers: vec![],
                body: body.into_bytes(),
            }),
            additional_responses: vec![],
        });
        self.outcalls += 1;
        for _ in 0..4 {
            pic.tick();
        }
    }

    /// Answers every outcall the canister makes until it asks for nothing more.
    fn drive(&mut self, pic: &PocketIc) {
        loop {
            let pending = pic.get_canister_http();
            if pending.is_empty() {
                return;
            }
            for request in pending {
                self.reply(pic, &request);
            }
        }
    }

    /// The first block of every log read, in the order they were asked.
    fn froms(&self) -> Vec<u64> {
        self.log_reads.iter().map(|(from, _)| *from).collect()
    }
}

/// The claim's first outcall, pending: the provider's head, asked alone, because every
/// window the claim reads is built from it.
struct HeadRead {
    request: CanisterHttpRequest,
    quote_hash: Hash32,
}

/// The one pending outcall, which must be the claim's first read: the provider's head,
/// asked alone before any window is built.
///
/// Rewritten for fix wave 5 (M1): the claim's first read was the head and the logs of its
/// one window in a batch, and it is now the head alone.
fn the_read(pic: &PocketIc, quote_hash: Hash32) -> HeadRead {
    let pending = pic.get_canister_http();
    assert_eq!(pending.len(), 1, "a claim has one outcall out at a time");
    let request = pending.into_iter().next().unwrap();
    assert_eq!(methods(&request), vec!["eth_blockNumber"]);
    HeadRead {
        request,
        quote_hash,
    }
}

/// Answers the claim's reads: the head at `latest`, and `logs` for the quote to every
/// window that reaches them, including those above `latest`, so a test can hand over a
/// log the head has not reached.
///
/// Rewritten for fix wave 5 (M1): the head is answered first and alone, and the windows
/// after it each get the logs in their range.
fn answer(pic: &PocketIc, read: &HeadRead, latest: u64, logs: Vec<Value>) {
    let mut provider = Provider {
        tip: u64::MAX,
        ..Provider::at(latest, &logs).for_quote(read.quote_hash)
    };
    provider.reply(pic, &read.request);
    provider.drive(pic);
}

/// The whole door, on the path that creates a swap: the claim reads the chain once, for
/// the vault's logs of the quote, and a deposit at depth becomes `FundsReceived`, carrying
/// what the vault logged.
#[test]
fn a_valid_mocked_deposit_appends_funds_received_and_returns_the_hash() {
    let (pic, canister, _admin) = setup();
    let quote = quote(1);
    let quote_hash = swap_id(&quote);
    let before = count(&pic, canister);

    let call = submit_claim(&pic, canister, watcher(), &quote);
    let read = the_read(&pic, quote_hash);
    answer(
        &pic,
        &read,
        HEAD,
        vec![deposit_log(quote_hash, HEAD, USDC, USER, AMOUNT.into())],
    );
    assert_eq!(await_claim(&pic, call), Ok(quote_hash));

    let log = events(&pic, canister);
    assert_eq!(log.len() as u64, before + 1, "one line: the swap");
    assert_eq!(
        log.last().unwrap().payload,
        EventType::FundsReceived {
            quote_hash,
            quote_bytes: quote.canonical_bytes().unwrap(),
            chain_id: BASE,
            token: USDC.to_string(),
            amount: Nat::from(AMOUNT),
            tx_ref: format!("0x{}", hex::encode([0x77; 32])),
        }
    );
    let swap =
        get_swap(&pic, canister, Principal::anonymous(), quote_hash).expect("the swap exists");
    assert_eq!(swap.status, SwapStatus::FundsReceived);
    assert!(verify_replay(&pic, canister, Principal::anonymous()));

    // the same claim again is refused before any outcall, and writes nothing
    assert_eq!(
        claim_swap(&pic, canister, watcher(), &wire(&quote)),
        Err(ClaimError::SwapExists(quote_hash))
    );
    assert!(pic.get_canister_http().is_empty());
    assert_eq!(count(&pic, canister), before + 1);
}

/// Money-first, proven by the count: a claim the chain does not back stores nothing. Neither
/// a vault with no log for the quote, nor one whose log is not deep enough yet.
///
/// Rewritten for fix wave 4 (N4): a claim that finds nothing at all above the height the
/// quote was registered at reads the plain lookback before it refuses, so the refusal
/// comes after every window of it, where it came after the one.
///
/// Rewritten for fix wave 5 (L4, N-d): the read starts twenty thousand blocks below the
/// height (three windows), and the fallback reads only the rest of the lookback, older than
/// that, where it read the whole lookback again.
///
/// Rewritten for fix wave 6 (M1): the default lookback is 400,000 blocks, so the rest of
/// it is 38 windows after the three, where it was 33.
#[test]
fn no_deposit_means_nothing_stored() {
    let (pic, canister, _admin) = setup();
    let quote = quote(2);
    let quote_hash = swap_id(&quote);
    let before = count(&pic, canister);

    let call = submit_claim(&pic, canister, watcher(), &quote);
    let reads = walk_the_reads(&pic, quote_hash, HEAD, &[]);
    assert_eq!(
        await_claim(&pic, call),
        Err(ClaimError::Deposit(DepositError::NotFound { quote_hash }))
    );
    assert_eq!(
        (reads[0], reads[3], reads.len()),
        (HEAD - 20_000, HEAD + 1 - 400_000, 41),
        "the margin below the registration height to the head, then the rest of the \
         lookback from its oldest"
    );
    assert_eq!(count(&pic, canister), before, "nothing stored");
    assert_eq!(
        get_swap(&pic, canister, Principal::anonymous(), quote_hash),
        None
    );

    // the deposit is there, in a block the head has not reached: not yet
    let call = submit_claim(&pic, canister, watcher(), &quote);
    answer(
        &pic,
        &the_read(&pic, quote_hash),
        HEAD,
        vec![deposit_log(quote_hash, HEAD + 3, USDC, USER, AMOUNT.into())],
    );
    assert_eq!(
        await_claim(&pic, call),
        Err(ClaimError::Deposit(DepositError::NotConfirmed {
            block: HEAD + 3,
            latest: HEAD,
            depth: 1,
        }))
    );
    assert_eq!(count(&pic, canister), before, "still nothing");

    // and once the head reaches it, the same claim goes through
    let call = submit_claim(&pic, canister, watcher(), &quote);
    answer(
        &pic,
        &the_read(&pic, quote_hash),
        HEAD + 3,
        vec![deposit_log(quote_hash, HEAD + 3, USDC, USER, AMOUNT.into())],
    );
    assert_eq!(await_claim(&pic, call), Ok(quote_hash));
    assert_eq!(count(&pic, canister), before + 1);
}

/// A deposit that is not the quote's, in token or in amount, is not the swap the user was
/// quoted, whoever made it: the claim finds no deposit it wants, says how many it saw so
/// an operator can tell stranded funds from none, stores nothing, and the funds stay in
/// the vault.
///
/// Rewritten for fix wave 4 (N4): a claim that finds nothing it wants above the height the
/// quote was registered at reads the plain lookback before it refuses, and the count is
/// that read's.
/// Rewritten for fix wave 6 (H1): the read asks for the quote's token alone, so a deposit
/// of another token under the hash is not in the answer at all, and the claim finds no
/// deposit, where it counted one it did not want.
#[test]
fn a_deposit_of_another_token_or_amount_is_refused_and_nothing_stored() {
    let (pic, canister, _admin) = setup();
    let quote = quote(3);
    let quote_hash = swap_id(&quote);
    let before = count(&pic, canister);

    let call = submit_claim(&pic, canister, watcher(), &quote);
    walk_the_reads(
        &pic,
        quote_hash,
        HEAD,
        &[deposit_log(quote_hash, HEAD, USER, USER, AMOUNT.into())],
    );
    assert_eq!(
        await_claim(&pic, call),
        Err(ClaimError::Deposit(DepositError::NotFound { quote_hash }))
    );

    let call = submit_claim(&pic, canister, watcher(), &quote);
    walk_the_reads(
        &pic,
        quote_hash,
        HEAD,
        &[deposit_log(
            quote_hash,
            HEAD,
            USDC,
            USER,
            u64::from(AMOUNT) - 1,
        )],
    );
    assert_eq!(
        await_claim(&pic, call),
        Err(ClaimError::Deposit(DepositError::NoneMatches {
            quote_hash,
            seen: 1
        }))
    );
    assert_eq!(count(&pic, canister), before, "nothing stored");
}

/// The vault marks a quote per payer, so anyone can log a deposit under the user's hash
/// ahead of theirs. The deposit that counts is the one that matches the quote, not the
/// first one logged: a dust deposit ahead of the real one changes nothing, and forty of
/// them still leave the real one claimed.
#[test]
fn a_dust_deposit_ahead_of_the_real_one_still_claims() {
    let (pic, canister, _admin) = setup();
    let quote = quote(14);
    let quote_hash = swap_id(&quote);

    let call = submit_claim(&pic, canister, watcher(), &quote);
    answer(
        &pic,
        &the_read(&pic, quote_hash),
        HEAD,
        vec![
            dust_log(quote_hash, HEAD - 2, 1),
            deposit_log(quote_hash, HEAD - 1, USDC, USER, AMOUNT.into()),
        ],
    );
    assert_eq!(await_claim(&pic, call), Ok(quote_hash));
    let EventType::FundsReceived { amount, tx_ref, .. } =
        events(&pic, canister).last().unwrap().payload.clone()
    else {
        panic!("the claim appends the swap");
    };
    assert_eq!(amount, Nat::from(AMOUNT));
    assert_eq!(
        tx_ref,
        format!("0x{}", hex::encode([0x77; 32])),
        "the real deposit's transaction, not the dust's"
    );

    let behind_forty = self::quote(15);
    let quote_hash = swap_id(&behind_forty);
    let mut logs: Vec<Value> = (0..40)
        .map(|i| dust_log(quote_hash, HEAD - 50 + i, 1 + i))
        .collect();
    logs.push(deposit_log(quote_hash, HEAD - 1, USDC, USER, AMOUNT.into()));
    let call = submit_claim(&pic, canister, watcher(), &behind_forty);
    answer(&pic, &the_read(&pic, quote_hash), HEAD, logs);
    assert_eq!(
        await_claim(&pic, call),
        Ok(quote_hash),
        "forty dust logs ahead change nothing"
    );
    assert!(verify_replay(&pic, canister, Principal::anonymous()));
}

/// The rails carry the configured USDC and nothing else: a quote naming a worthless token
/// on either side is refused by the field before an outcall is bought, so the vault's
/// USDC can never be burned against a deposit of something else. And the fold holds the
/// line that creates a swap to the quote it carries: a hand-written `FundsReceived`
/// naming another amount, token or chain than its quote's is refused at the append.
///
/// Extended for the hardening pass (H4, review 5 M3): the store refuses the same quotes
/// on the same check before a user is handed them to pay (rule A5), and stores nothing.
#[test]
fn a_quote_naming_a_worthless_token_is_refused_before_any_outcall() {
    use crate::client::settlement::append;
    use settlement_api::types::errors::{AppendError, TestAppendError};
    use settlement_api::types::quote::{QuoteAddressField, RailTokenError};
    use settlement_api::types::swap::TransitionError;
    let (pic, canister, admin) = setup();
    let worthless = types::Quote {
        src_token: USER.parse().unwrap(),
        ..quote(16)
    };
    assert_eq!(
        claim_swap(&pic, canister, watcher(), &wire(&worthless)),
        Err(ClaimError::RailToken(RailTokenError::NotTheRailToken {
            field: QuoteAddressField::SrcToken,
            quoted: USER.to_string(),
            rail_token: USDC.to_string(),
            rail: "cctp_v2_fast".to_string(),
        }))
    );
    let wrong_destination = types::Quote {
        dst_token: USER.parse().unwrap(),
        ..quote(17)
    };
    assert_eq!(
        claim_swap(&pic, canister, watcher(), &wire(&wrong_destination)),
        Err(ClaimError::RailToken(RailTokenError::NotTheRailToken {
            field: QuoteAddressField::DstToken,
            quoted: USER.to_string(),
            rail_token: USDC.to_string(),
            rail: "cctp_v2_fast".to_string(),
        }))
    );
    assert!(
        pic.get_canister_http().is_empty(),
        "refused before any outcall"
    );
    use settlement_api::types::errors::RegisterQuoteError;
    for (quote, field) in [
        (&worthless, QuoteAddressField::SrcToken),
        (&wrong_destination, QuoteAddressField::DstToken),
    ] {
        assert_eq!(
            register_quote(&pic, canister, quoter(), &wire(quote)),
            Err(RegisterQuoteError::RailToken(
                RailTokenError::NotTheRailToken {
                    field,
                    quoted: USER.to_string(),
                    rail_token: USDC.to_string(),
                    rail: "cctp_v2_fast".to_string(),
                }
            )),
            "the store refuses what the claim refuses: {field:?}"
        );
        assert_eq!(
            get_pending(&pic, canister, watcher(), swap_id(quote)).unwrap(),
            None,
            "and stores nothing"
        );
    }

    let quote = quote(18);
    let quote_hash = swap_id(&quote);
    let before = count(&pic, canister);
    let short = EventType::FundsReceived {
        quote_hash,
        quote_bytes: quote.canonical_bytes().unwrap(),
        chain_id: BASE,
        token: USDC.to_string(),
        amount: Nat::from(AMOUNT - 1),
        tx_ref: "0xhand".into(),
    };
    assert_eq!(
        append(&pic, canister, admin, &short),
        Err(TestAppendError::Append(AppendError::Transition(
            TransitionError::FundsAmountNotTheQuotes {
                logged: Nat::from(AMOUNT - 1),
                quoted: Nat::from(AMOUNT),
            }
        )))
    );
    let elsewhere = EventType::FundsReceived {
        quote_hash,
        quote_bytes: quote.canonical_bytes().unwrap(),
        chain_id: ARBITRUM,
        token: USDC.to_string(),
        amount: Nat::from(AMOUNT),
        tx_ref: "0xhand".into(),
    };
    assert_eq!(
        append(&pic, canister, admin, &elsewhere),
        Err(TestAppendError::Append(AppendError::Transition(
            TransitionError::FundsChainNotTheQuotes {
                logged: ARBITRUM,
                quoted: BASE,
            }
        )))
    );
    assert_eq!(count(&pic, canister), before, "nothing written");
}

/// The sanctions gate runs before an outcall is bought: a quote paying to a sanctioned
/// destination or refund address is refused with no request pending. A sanctioned payer is
/// only known once the log is read, and is refused then, with nothing stored.
///
/// Fix wave 4 (N6) moved the fixture's destination from the text `0xuser` to an address,
/// so the destination this test lists is that address.
/// Rewritten for the hardening pass (H1): the payer is the quote's refund address, so a
/// payer listed before the claim is refused as the refund address before any outcall. The
/// check after the read is what catches the payer listed while the read was out, which
/// is the case this test now lists it in.
#[test]
fn a_sanctioned_party_is_refused_and_the_destination_before_any_outcall() {
    let (pic, canister, _admin) = setup();
    let quote = quote(4);
    let quote_hash = swap_id(&quote);
    let before = count(&pic, canister);

    set_sanctioned(&pic, canister, watcher(), &[DST], &[]).unwrap();
    assert_eq!(
        claim_swap(&pic, canister, watcher(), &wire(&quote)),
        Err(ClaimError::Sanctioned {
            party: "dst_address".to_string()
        })
    );
    assert!(
        pic.get_canister_http().is_empty(),
        "refused before any outcall"
    );
    set_sanctioned(&pic, canister, watcher(), &[USER], &[DST]).unwrap();
    assert_eq!(
        claim_swap(&pic, canister, watcher(), &wire(&quote)),
        Err(ClaimError::Sanctioned {
            party: "refund_address".to_string()
        })
    );
    assert!(pic.get_canister_http().is_empty());

    // the payer listed while the read is out: the read happens, and then the refusal. The
    // payer is an EVM address, so its spelling does not matter
    set_sanctioned(&pic, canister, watcher(), &[], &[USER]).unwrap();
    let call = submit_claim(&pic, canister, watcher(), &quote);
    let read = the_read(&pic, quote_hash);
    set_sanctioned(
        &pic,
        canister,
        watcher(),
        &[&USER.to_ascii_lowercase()],
        &[],
    )
    .unwrap();
    answer(
        &pic,
        &read,
        HEAD,
        vec![deposit_log(quote_hash, HEAD, USDC, USER, AMOUNT.into())],
    );
    assert_eq!(
        await_claim(&pic, call),
        Err(ClaimError::Sanctioned {
            party: "from".to_string()
        })
    );
    assert_eq!(count(&pic, canister), before, "nothing stored");
}

/// Rule A8: two claims for one quote that are both in flight buy one set of reads. The
/// later one finds the earlier one's marker and is refused at once, and the earlier one
/// goes on to create the swap.
///
/// Rewritten for the hardening pass: it assumed the subnet runs the two messages in the
/// order they were submitted, which the hardening commit's bigger wasm reversed; it now
/// holds the rule whichever message runs first.
#[test]
fn concurrent_claims_buy_one_outcall() {
    let (pic, canister, _admin) = setup();
    let quote = quote(5);
    let quote_hash = swap_id(&quote);
    registered(&pic, canister, &quote);

    let first = pic
        .submit_call(
            canister,
            watcher(),
            "claim_swap",
            encode_one(wire(&quote)).unwrap(),
        )
        .unwrap();
    let second = pic
        .submit_call(
            canister,
            quoter(),
            "claim_swap",
            encode_one(wire(&quote)).unwrap(),
        )
        .unwrap();
    pic.tick();
    pic.tick();
    let read = the_read(&pic, quote_hash);
    // which of the two messages the subnet runs first is the induction order, not the
    // order they were submitted in (a bigger wasm moved it once already): the rule is
    // that one takes the marker and buys the one outcall, and the other, already
    // answered without an outcall, is refused by that marker, whichever comes first
    let (holder, refused) = match (
        pic.ingress_status(first.clone()),
        pic.ingress_status(second.clone()),
    ) {
        (None, Some(done)) => (first, done),
        (Some(done), None) => (second, done),
        other => panic!("exactly one claim is answered before the read returns: {other:?}"),
    };
    let refused: Result<Hash32, ClaimError> =
        candid::decode_one(&refused.expect("the refused claim returns")).unwrap();
    assert!(
        matches!(refused, Err(ClaimError::InFlight { .. })),
        "the later claim is refused by the first's marker: {refused:?}"
    );
    answer(
        &pic,
        &read,
        HEAD,
        vec![deposit_log(quote_hash, HEAD, USDC, USER, AMOUNT.into())],
    );
    assert_eq!(await_claim(&pic, holder), Ok(quote_hash));

    // and the marker went with the message chain: the quote is not held after it
    assert_eq!(
        claim_swap(&pic, canister, watcher(), &wire(&quote)),
        Err(ClaimError::SwapExists(quote_hash)),
        "refused by the swap now, not by a marker"
    );
}

/// A quote no claim is admitted for any more is not claimed: past its expiry, the permit
/// window and the grace after it, the claim is refused before any outcall. Inside the
/// window it still is.
///
/// Rewritten for fix wave 4 (N4): the claim inside the window finds nothing above the
/// registration height and reads the plain lookback before it refuses.
///
/// Rewritten for fix wave 5 (N7): the claim is refused once the grace after the permit
/// window has ended, where it was refused as soon as the permit window closed.
#[test]
fn an_expired_quote_is_refused_before_any_outcall() {
    let (pic, canister, _admin) = setup();
    // the quote expires a minute out, and the quoter registers it now: the store takes no
    // quote that has already expired, so the registration is what fixes the clock here
    let quote = types::Quote {
        expires_at: UnixSeconds::new(
            pic.get_time().as_nanos_since_unix_epoch() / 1_000_000_000 + 60,
        ),
        ..quote(6)
    };
    registered(&pic, canister, &quote);
    // the default permit window is two minutes: at the expiry itself, and a minute past
    // it, the quote is still claimable
    pic.advance_time(Duration::from_secs(60));
    push_reading(&pic, canister);
    let call = submit_claim_unregistered(&pic, canister, watcher(), &wire(&quote));
    walk_the_reads(&pic, swap_id(&quote), HEAD, &[]);
    assert!(matches!(
        await_claim(&pic, call),
        Err(ClaimError::Deposit(DepositError::NotFound { .. }))
    ));

    // the clock is at the expiry: past the two minute permit window and the hour of
    // grace after it
    pic.advance_time(Duration::from_secs(120 + 3_600 + 1));
    push_reading(&pic, canister);
    let refused = claim_swap(&pic, canister, watcher(), &wire(&quote));
    assert!(
        matches!(refused, Err(ClaimError::QuoteExpired { .. })),
        "{refused:?}"
    );
    assert!(
        pic.get_canister_http().is_empty(),
        "refused before any outcall"
    );
}

/// The door is the services', and the halt switch closes it: neither a stranger nor a
/// halted canister reads a chain for a claim.
#[test]
fn a_stranger_and_a_halted_canister_claim_nothing() {
    let (pic, canister, admin) = setup();
    let quote = wire(&quote(7));
    assert_eq!(
        claim_swap(&pic, canister, Principal::from_slice(&[9; 29]), &quote),
        // the door admits a controller too, so the refusal names all three
        Err(ClaimError::Guard(
            GuardError::CallerNotQuoterWatcherOrController
        ))
    );
    set_halted(&pic, canister, admin, true).unwrap();
    assert_eq!(
        claim_swap(&pic, canister, watcher(), &quote),
        Err(ClaimError::Guard(GuardError::Halted))
    );
    assert!(pic.get_canister_http().is_empty());
}

/// The inbox is the watcher's, has a slot only for a known swap whose burn has confirmed,
/// holds a message to its bounds, and takes nothing for a swap with no burn to attest.
/// The push that binds, and the same push twice, are proven end to end in Phase 0.
#[test]
fn push_attestation_is_the_watchers_and_needs_a_known_swap() {
    let (pic, canister, _admin) = setup();
    let quote = quote(8);
    let quote_hash = swap_id(&quote);
    let message = vec![0xaa; 376];
    let attestation = vec![0xbb; 65];
    let burn = [0x77; 32];

    assert_eq!(
        push_attestation(
            &pic,
            canister,
            Principal::from_slice(&[9; 29]),
            quote_hash,
            burn,
            &message,
            &attestation
        ),
        Err(PushAttestationError::Guard(GuardError::CallerNotRole(
            settlement_api::types::errors::Role::Watcher
        )))
    );
    assert_eq!(
        push_attestation(
            &pic,
            canister,
            watcher(),
            quote_hash,
            burn,
            &message,
            &attestation
        ),
        Err(PushAttestationError::UnknownSwap(quote_hash)),
        "no swap, no inbox slot"
    );

    let call = submit_claim(&pic, canister, watcher(), &quote);
    answer(
        &pic,
        &the_read(&pic, quote_hash),
        HEAD,
        vec![deposit_log(quote_hash, HEAD, USDC, USER, AMOUNT.into())],
    );
    assert_eq!(await_claim(&pic, call), Ok(quote_hash));
    let before = count(&pic, canister);
    assert_eq!(
        push_attestation(
            &pic,
            canister,
            watcher(),
            quote_hash,
            burn,
            &vec![0; 4_097],
            &attestation
        ),
        Err(PushAttestationError::MessageTooLong {
            len: 4_097,
            cap: 4_096
        })
    );
    assert_eq!(
        push_attestation(
            &pic,
            canister,
            watcher(),
            quote_hash,
            burn,
            &message,
            &attestation
        ),
        Err(PushAttestationError::NoBurnConfirmed(quote_hash)),
        "a swap whose burn has not confirmed has no burn to attest"
    );
    assert_eq!(
        count(&pic, canister),
        before,
        "rail data is not a line in the log"
    );
}

/// The token pin is the door's and the rails', never the fold's: the fold is the replay of
/// the log, and a config the fold read would make the log's own history unreplayable the
/// day a table moves. So a moved USDC table refuses every new claim and leaves every swap
/// already recorded replaying clean.
#[test]
fn a_moved_token_table_refuses_new_claims_and_leaves_the_log_replayable() {
    use crate::client::settlement::set_config;
    use settlement_api::types::quote::RailTokenError;
    let (pic, canister, admin) = setup();
    let quote = quote(19);
    let quote_hash = swap_id(&quote);
    let call = submit_claim(&pic, canister, watcher(), &quote);
    answer(
        &pic,
        &the_read(&pic, quote_hash),
        HEAD,
        vec![deposit_log(quote_hash, HEAD, USDC, USER, AMOUNT.into())],
    );
    assert_eq!(await_claim(&pic, call), Ok(quote_hash));

    let moved = Config {
        usdc_addresses: BTreeMap::from([(BASE, USER.to_string()), (ARBITRUM, USER.to_string())]),
        ..config()
    };
    set_config(&pic, canister, admin, &moved).expect("the controller moves the table");
    assert!(
        matches!(
            claim_swap(&pic, canister, watcher(), &wire(&self::quote(20))),
            Err(ClaimError::RailToken(
                RailTokenError::NotTheRailToken { .. }
            ))
        ),
        "a claim against the moved table is refused"
    );
    assert!(
        verify_replay(&pic, canister, Principal::anonymous()),
        "and the swap recorded under the old table still replays"
    );
}

/// The quote at `nonce`, expiring ten minutes from the network's clock, so the pending
/// store takes it.
fn live_quote(pic: &PocketIc, nonce: u64) -> types::Quote {
    let now_s = pic.get_time().as_nanos_since_unix_epoch() / 1_000_000_000;
    types::Quote {
        expires_at: UnixSeconds::new(now_s + 600),
        ..quote(nonce)
    }
}

/// The Permit2 permit a gasless user signs for the vault: witnessed by the quote it pays,
/// for the quote's token and amount, with the vault as the spender.
fn permit(quote: &types::Quote) -> PullRequest {
    PullRequest::Permit2(Permit2Sig {
        quote_hash: swap_id(quote),
        token: USDC.to_string(),
        owner: USER.to_string(),
        spender: VAULT.to_string(),
        amount: quote.amount_in.into(),
        nonce: Nat::from(7_u8),
        deadline_s: EXPIRES_AT + 100,
        signature: vec![0x22; 65],
    })
}

/// What the permit above says, as the pull encodes it.
fn permitted(quote: &types::Quote) -> types::abi::Permit2Permit {
    types::abi::Permit2Permit {
        token: USDC.parse().unwrap(),
        amount: quote.amount_in,
        nonce: types::Permit2Nonce::from(7_u8),
        deadline: UnixSeconds::new(EXPIRES_AT + 100),
    }
}

/// A pull is for a pending quote and nothing else: one never registered is refused, one
/// whose user pays their own gas is refused, and a stranger is refused, all before
/// anything is signed.
#[test]
fn a_pull_for_an_unknown_or_legacy_quote_is_refused() {
    let (pic, canister, _admin) = setup();
    let unknown = quote(9);
    assert_eq!(
        start_gasless_pull(
            &pic,
            canister,
            quoter(),
            swap_id(&unknown),
            &permit(&unknown)
        ),
        Err(PullError::UnknownQuote(swap_id(&unknown)))
    );
    let legacy = live_quote(&pic, 10);
    register_quote(&pic, canister, quoter(), &wire(&legacy)).expect("the quoter registers");
    assert_eq!(
        start_gasless_pull(&pic, canister, quoter(), swap_id(&legacy), &permit(&legacy)),
        Err(PullError::NotGasless)
    );
    assert_eq!(
        start_gasless_pull(
            &pic,
            canister,
            watcher(),
            swap_id(&legacy),
            &permit(&legacy)
        ),
        Err(PullError::Guard(GuardError::CallerNotRole(
            settlement_api::types::errors::Role::Quoter
        )))
    );
    assert!(
        events(&pic, canister)
            .iter()
            .all(|event| !matches!(event.payload, EventType::TxCreated { .. })),
        "nothing was allocated"
    );
}

/// A pull goes through the one send path: the nonce is allocated, the bytes are recorded
/// as `PullSigned` before they are broadcast, and what goes out is the vault's
/// `pullWithPermit2`, witnessed by the quote. A second pull while the first is on its way is refused, and no swap
/// exists until a claim verifies the deposit the pull made.
///
/// Extended for fix wave 6 (M2): a permit good past the deposit deadline is refused by
/// name before anything is allocated.
/// Extended for the hardening pass (H1): a permit signed by another wallet than the
/// quote's refund address is refused by name before anything is allocated.
#[test]
fn a_pending_gasless_quote_is_pulled_through_the_send_path() {
    let (pic, canister, _admin) = setup_with_keys();
    let quote = types::Quote {
        gas_mode: GasMode::Gasless,
        ..live_quote(&pic, 11)
    };
    let quote_hash = swap_id(&quote);
    register_quote(&pic, canister, quoter(), &wire(&quote)).expect("the quoter registers");

    let PullRequest::Permit2(signed) = permit(&quote) else {
        panic!("the fixture is a Permit2 permit");
    };
    assert_eq!(
        start_gasless_pull(
            &pic,
            canister,
            quoter(),
            quote_hash,
            &PullRequest::Permit2(Permit2Sig {
                amount: Nat::from(1_u8),
                ..signed.clone()
            })
        ),
        Err(PullError::PermitMismatch(PermitMismatch::Amount {
            permitted: Nat::from(1_u8),
            wanted: quote.amount_in.into(),
        }))
    );
    // a permit witnessed for another quote frees nothing here, whatever else it says
    let elsewhere = swap_id(&self::quote(12));
    assert_eq!(
        start_gasless_pull(
            &pic,
            canister,
            quoter(),
            quote_hash,
            &PullRequest::Permit2(Permit2Sig {
                quote_hash: elsewhere,
                ..signed.clone()
            })
        ),
        Err(PullError::PermitMismatch(PermitMismatch::Witness {
            signed_for: elsewhere
        }))
    );
    // a permit signed by another wallet than the quote's refund address would land funds
    // the vault logs as that wallet's, which no claim counts as the quote's deposit
    assert_eq!(
        start_gasless_pull(
            &pic,
            canister,
            quoter(),
            quote_hash,
            &PullRequest::Permit2(Permit2Sig {
                owner: STRANGER.to_string(),
                ..signed.clone()
            })
        ),
        Err(PullError::PermitMismatch(PermitMismatch::Owner {
            signed_by: STRANGER.to_string(),
            refund_address: USER.to_string(),
        }))
    );
    // and the 2612 door is not one this canister pulls through
    assert_eq!(
        start_gasless_pull(
            &pic,
            canister,
            quoter(),
            quote_hash,
            &PullRequest::Eip2612(PermitSig {
                token: USDC.to_string(),
                owner: USER.to_string(),
                amount: quote.amount_in.into(),
                deadline_s: EXPIRES_AT + 100,
                v: 28,
                r: [0x22; 32],
                s: [0x33; 32],
            })
        ),
        Err(PullError::Permit(PermitError::NotAPermit2Permit))
    );
    // a permit good past the deposit deadline would let a late pull land a deposit the
    // claim refuses (review 5, M2)
    let deposit_until = quote.expires_at.get() + 120;
    assert_eq!(
        start_gasless_pull(
            &pic,
            canister,
            quoter(),
            quote_hash,
            &PullRequest::Permit2(Permit2Sig {
                deadline_s: deposit_until + 1,
                ..signed.clone()
            })
        ),
        Err(PullError::PermitMismatch(PermitMismatch::OutlastsDeposit {
            deadline_s: deposit_until + 1,
            deposit_until_s: deposit_until,
        }))
    );

    let tx_hash = start_gasless_pull(&pic, canister, quoter(), quote_hash, &permit(&quote))
        .expect("the pull is signed and queued");
    let log = events(&pic, canister);
    let created: Vec<TxPurpose> = log
        .iter()
        .filter_map(|event| match &event.payload {
            EventType::TxCreated { purpose, .. } => Some(*purpose),
            _ => None,
        })
        .collect();
    assert_eq!(created, vec![TxPurpose::GaslessPull(quote_hash)]);
    let signed: Vec<Vec<u8>> = log
        .iter()
        .filter_map(|event| match &event.payload {
            EventType::PullSigned {
                quote_hash: named,
                nonce,
                tx_hash: hash,
                raw_tx,
                ..
            } => {
                assert_eq!(*named, quote_hash);
                assert_eq!(*nonce, 0);
                assert_eq!(*hash, tx_hash);
                Some(raw_tx.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(signed.len(), 1, "one signed record, the pull's own");
    assert!(
        hex::encode(&signed[0]).contains(&hex::encode(types::abi::vault_pull_with_permit2(
            types::QuoteHash::new(quote_hash),
            USER.parse().unwrap(),
            &permitted(&quote),
            &[0x22; 65],
        ))),
        "the bytes carry the Permit2 pull this permit makes, field for field"
    );
    assert!(
        log.iter()
            .all(|event| !matches!(event.payload, EventType::TxSigned { .. })),
        "a pull is no swap's attempt"
    );
    assert_eq!(
        get_swap(&pic, canister, Principal::anonymous(), quote_hash),
        None,
        "a pull creates no swap"
    );

    // a second pull while the first is on its way is refused
    assert_eq!(
        start_gasless_pull(&pic, canister, quoter(), quote_hash, &permit(&quote)),
        Err(PullError::AlreadyPulling { tx_hash })
    );

    // the bytes broadcast are the bytes the log recorded
    pic.advance_time(Duration::from_secs(2));
    push_reading(&pic, canister);
    for _ in 0..4 {
        pic.tick();
    }
    let pending = pic.get_canister_http();
    assert_eq!(pending.len(), 1, "one chain, one batch");
    assert_eq!(methods(&pending[0]), vec!["eth_sendRawTransaction"]);
    assert_eq!(
        params(&pending[0], 0),
        json!([format!("0x{}", hex::encode(&signed[0]))])
    );
    assert!(verify_replay(&pic, canister, Principal::anonymous()));
}

/// The Eco rail is off until its route is designed: a quote naming it is refused at the
/// claim before an outcall is bought, so no Eco swap exists to push an intent for, and the
/// inbox door is the watcher's and refuses a swap on another rail. With the rail on, the
/// intent lands, the same push again changes nothing, and a push once the publish has been
/// signed is refused: the intent the Portal holds is the one the reclaim must name.
#[test]
fn eco_is_off_until_its_route_is_designed_and_an_intent_is_the_publishs_own() {
    use crate::client::settlement::{append, set_config};
    use settlement_api::types::entry::{EcoIntent, PushEcoIntentError};
    use settlement_api::types::events::EvmAddressError;
    let (pic, canister, admin) = setup();
    let intent = EcoIntent {
        destination_chain: BASE,
        route: vec![0xde, 0xad, 0xbe, 0xef],
        deadline_s: 1_800_000_500,
        prover: "0xeC00008537c1F26E739486BCFCC818d81234d5aD".to_string(),
    };
    let eco = types::Quote {
        rail: Rail::Eco,
        ..quote(13)
    };
    assert_eq!(
        claim_swap(&pic, canister, watcher(), &wire(&eco)),
        Err(ClaimError::RailUnavailable {
            rail: "eco".to_string()
        }),
        "the rail is off on a deploy"
    );
    assert!(
        pic.get_canister_http().is_empty(),
        "refused before any outcall"
    );

    // a CCTP swap, which no intent may steer onto a publish
    let cctp = quote(12);
    let cctp_hash = swap_id(&cctp);
    let call = submit_claim(&pic, canister, watcher(), &cctp);
    answer(
        &pic,
        &the_read(&pic, cctp_hash),
        HEAD,
        vec![deposit_log(cctp_hash, HEAD, USDC, USER, AMOUNT.into())],
    );
    assert_eq!(await_claim(&pic, call), Ok(cctp_hash));
    assert_eq!(
        push_eco_intent(&pic, canister, watcher(), cctp_hash, &intent),
        Err(PushEcoIntentError::NotAnEcoSwap(cctp_hash))
    );

    // the deploy turns the rail on, and the Eco quote is claimed like any other
    set_config(
        &pic,
        canister,
        admin,
        &Config {
            eco_enabled: true,
            ..config()
        },
    )
    .expect("the controller turns the rail on");
    let eco_hash = swap_id(&eco);
    assert_eq!(
        push_eco_intent(&pic, canister, watcher(), eco_hash, &intent),
        Err(PushEcoIntentError::UnknownSwap(eco_hash)),
        "no swap, no inbox slot"
    );
    let call = submit_claim(&pic, canister, watcher(), &eco);
    answer(
        &pic,
        &the_read(&pic, eco_hash),
        HEAD,
        vec![deposit_log(eco_hash, HEAD, USDC, USER, AMOUNT.into())],
    );
    assert_eq!(await_claim(&pic, call), Ok(eco_hash));

    assert_eq!(
        push_eco_intent(
            &pic,
            canister,
            Principal::from_slice(&[9; 29]),
            eco_hash,
            &intent
        ),
        Err(PushEcoIntentError::Guard(GuardError::CallerNotRole(
            settlement_api::types::errors::Role::Watcher
        )))
    );
    for _ in 0..2 {
        assert_eq!(
            push_eco_intent(&pic, canister, watcher(), eco_hash, &intent),
            Ok(()),
            "the intent lands, and the same push again changes nothing"
        );
    }
    let bad_prover = EcoIntent {
        prover: "prover".to_string(),
        ..intent.clone()
    };
    assert_eq!(
        push_eco_intent(&pic, canister, watcher(), eco_hash, &bad_prover),
        Err(PushEcoIntentError::ProverNotAnAddress {
            reason: EvmAddressError::NoPrefix
        })
    );
    let long_route = EcoIntent {
        route: vec![0; 8_193],
        ..intent.clone()
    };
    assert_eq!(
        push_eco_intent(&pic, canister, watcher(), eco_hash, &long_route),
        Err(PushEcoIntentError::RouteTooLong {
            len: 8_193,
            cap: 8_192
        })
    );

    // the publish is signed: from here the intent the Portal holds is the swap's own
    append(
        &pic,
        canister,
        admin,
        &EventType::TxCreated {
            purpose: TxPurpose::Burn(eco_hash),
            chain_id: BASE,
            nonce: 0,
            to: VAULT.to_string(),
            value_wei: Nat::from(0_u8),
            data: vec![],
            gas_limit: Nat::from(450_000_u32),
            max_fee_wei_per_gas: Nat::from(2_000_000_000_u64),
            max_priority_fee_wei_per_gas: Nat::from(100_000_000_u64),
        },
    )
    .expect("the allocation is admitted");
    append(
        &pic,
        canister,
        admin,
        &EventType::TxSigned {
            quote_hash: eco_hash,
            attempt: 1,
            chain_id: BASE,
            tx_hash: [0x55; 32],
            raw_tx: vec![0x02],
        },
    )
    .expect("the publish is signed");
    let moved = EcoIntent {
        deadline_s: 1_900_000_000,
        ..intent
    };
    assert_eq!(
        push_eco_intent(&pic, canister, watcher(), eco_hash, &moved),
        Err(PushEcoIntentError::AlreadyPublished(eco_hash)),
        "the intent the publish carries is the one the reclaim names"
    );
    assert!(verify_replay(&pic, canister, Principal::anonymous()));
}

/// A quote no refund could ever be paid on is refused at both doors, before anything is
/// read or stored: the fold does not hold the payer, so the refund address is the only way
/// the user's funds come back, and a swap without one could only freeze with them in the
/// vault.
#[test]
fn a_quote_naming_no_refund_address_is_refused_at_both_doors() {
    use crate::client::settlement::register_quote;
    use settlement_api::types::errors::RegisterQuoteError;
    use settlement_api::types::quote::{QuoteAddressError, QuoteAddressField};
    let (pic, canister, _admin) = setup();
    let none = types::Quote {
        refund_address: None,
        ..live_quote(&pic, 21)
    };
    assert_eq!(
        register_quote(&pic, canister, quoter(), &wire(&none)),
        Err(RegisterQuoteError::NoRefundAddress)
    );
    assert_eq!(
        claim_swap(&pic, canister, watcher(), &wire(&none)),
        Err(ClaimError::QuoteAddress(QuoteAddressError::Absent {
            field: QuoteAddressField::RefundAddress
        }))
    );
    let not_an_address = types::Quote {
        refund_address: Some("0xrefund".parse().unwrap()),
        ..live_quote(&pic, 22)
    };
    assert_eq!(
        register_quote(&pic, canister, quoter(), &wire(&not_an_address)),
        Err(RegisterQuoteError::RefundAddressNotAnAddress {
            reason: settlement_api::types::events::EvmAddressError::WrongLength { len: 6 }
        })
    );
    assert!(
        pic.get_canister_http().is_empty(),
        "refused before any outcall"
    );
    assert_eq!(
        count(&pic, canister),
        2,
        "the install's two lines, and no more"
    );
}

/// The halt is read again after the read comes back: a claim whose outcall was already out
/// when the operator halted records nothing, so no swap appears while everything is
/// supposed to be stopped, and the same claim goes through once the halt lifts.
#[test]
fn a_claim_whose_read_returns_after_a_halt_records_nothing() {
    let (pic, canister, admin) = setup();
    let quote = quote(23);
    let quote_hash = swap_id(&quote);
    let before = count(&pic, canister);

    let call = submit_claim(&pic, canister, watcher(), &quote);
    let read = the_read(&pic, quote_hash);
    set_halted(&pic, canister, admin, true).expect("the operator halts");
    answer(
        &pic,
        &read,
        HEAD,
        vec![deposit_log(quote_hash, HEAD, USDC, USER, AMOUNT.into())],
    );
    assert_eq!(
        await_claim(&pic, call),
        Err(ClaimError::Guard(GuardError::Halted))
    );
    assert_eq!(count(&pic, canister), before, "nothing was recorded");
    assert_eq!(
        get_swap(&pic, canister, Principal::anonymous(), quote_hash),
        None
    );

    set_halted(&pic, canister, admin, false).expect("the operator lifts the halt");
    let call = submit_claim(&pic, canister, watcher(), &quote);
    answer(
        &pic,
        &the_read(&pic, quote_hash),
        HEAD,
        vec![deposit_log(quote_hash, HEAD, USDC, USER, AMOUNT.into())],
    );
    assert_eq!(await_claim(&pic, call), Ok(quote_hash));
}

/// A swap's economics are the quoter's: the rail, the destination, the expiry and the
/// amounts all come from the quote, so a claim is admitted only for a quote the quoter
/// registered. The watcher alone cannot author one and claim its own deposit. A controller
/// stands in where a deposit has to be claimed by hand, which is the ops path for a quote
/// the store no longer holds.
#[test]
fn a_claim_needs_a_quote_the_quoter_registered() {
    let (pic, canister, admin) = setup();
    let unregistered = quote(30);
    let quote_hash = swap_id(&unregistered);
    assert_eq!(
        claim_swap(&pic, canister, watcher(), &wire(&unregistered)),
        Err(ClaimError::NotRegistered(quote_hash))
    );
    assert!(
        pic.get_canister_http().is_empty(),
        "refused before any outcall"
    );
    assert_eq!(
        get_swap(&pic, canister, Principal::anonymous(), quote_hash),
        None
    );

    // the quoter registers it, and the same claim goes through
    registered(&pic, canister, &unregistered);
    let call = submit_claim_unregistered(&pic, canister, watcher(), &wire(&unregistered));
    answer(
        &pic,
        &the_read(&pic, quote_hash),
        HEAD,
        vec![deposit_log(quote_hash, HEAD, USDC, USER, AMOUNT.into())],
    );
    assert_eq!(await_claim(&pic, call), Ok(quote_hash));

    // and the operator's door: a quote the store never held, claimed by a controller
    let by_hand = quote(31);
    let by_hand_hash = swap_id(&by_hand);
    let call = submit_claim_unregistered(&pic, canister, admin, &wire(&by_hand));
    answer(
        &pic,
        &the_read(&pic, by_hand_hash),
        HEAD,
        vec![deposit_log(by_hand_hash, HEAD, USDC, USER, AMOUNT.into())],
    );
    assert_eq!(await_claim(&pic, call), Ok(by_hand_hash));
    assert!(verify_replay(&pic, canister, Principal::anonymous()));
}

/// The deposit that pays a quote is in no block before the quote was registered, so the
/// claim's log read starts at the height the store kept, less the margin a watcher's
/// height is trusted to, and reads a few windows. A quote the store holds no height for is
/// read from the lookback, which is a day of blocks on the chain whose blocks come
/// fastest: a deposit made while the canister was halted is still in the range.
///
/// Rewritten for fix wave 4 (N2): the lookback is now walked from its oldest window
/// forward, since the deposit that counts is the oldest deep one in range, so the
/// controller's claim reads every window up to the one holding the deposit instead of
/// starting at the newest.
///
/// Rewritten for fix wave 5 (L4, M1): the read starts twenty thousand blocks (the
/// margin) below the height, where it started at the height itself, and the provider's
/// head is asked first, alone, and is no log read.
/// Rewritten for fix wave 6 (M1): the controller's read covers the default lookback of
/// 400,000 blocks, forty windows, where it covered 345,600 in 35.
#[test]
fn the_read_starts_at_the_block_the_quote_was_registered_at() {
    let (pic, canister, admin) = setup();
    let quote = quote(32);
    let quote_hash = swap_id(&quote);
    registered(&pic, canister, &quote);
    // the watcher's head moves on before the claim, and the read still starts where the
    // quote was registered
    push_chain_data(
        &pic,
        canister,
        watcher(),
        BASE,
        &ChainData {
            block: HEAD + 5_000,
            base_fee_wei_per_gas: Nat::from(1_000_000_000_u64),
            priority_fee_wei_per_gas: Nat::from(100_000_000_u64),
        },
    )
    .expect("the watcher may push");
    // the quoter retries the same quote from the head it sees now: the height the claim
    // reads from stays the earliest one, because the user may have deposited in between
    registered(&pic, canister, &quote);
    let call = submit_claim_unregistered(&pic, canister, watcher(), &wire(&quote));
    let reads = walk_the_reads(
        &pic,
        quote_hash,
        HEAD + 5_000,
        &[deposit_log(quote_hash, HEAD + 1, USDC, USER, AMOUNT.into())],
    );
    assert_eq!(await_claim(&pic, call), Ok(quote_hash));
    assert_eq!(
        reads.first(),
        Some(&(HEAD - 20_000)),
        "the margin below the height the quote was first registered at, not a day of \
         blocks back and not the head a retry saw"
    );

    // a quote the store holds nothing for: the controller's door, read from the lookback,
    // oldest window first, up to the window that holds the deposit
    let by_hand = self::quote(33);
    let by_hand_hash = swap_id(&by_hand);
    let call = submit_claim_unregistered(&pic, canister, admin, &wire(&by_hand));
    let reads = walk_the_reads(
        &pic,
        by_hand_hash,
        HEAD + 5_000,
        &[deposit_log(
            by_hand_hash,
            HEAD + 4_000,
            USDC,
            USER,
            AMOUNT.into(),
        )],
    );
    assert_eq!(await_claim(&pic, call), Ok(by_hand_hash));
    assert_eq!(
        reads.first(),
        Some(&(HEAD + 5_000 + 1 - 400_000)),
        "the oldest window of the lookback is read first"
    );
    assert_eq!(
        reads.len(),
        40,
        "and every window after it, up to the newest, which holds the deposit"
    );
}

/// Answers every read a claim makes from a provider at `latest` holding `logs`, until it
/// asks for nothing more, and gives back the block each log read started at, in the order
/// they were asked.
///
/// Rewritten for fix wave 5 (M1): the provider's head is asked first and alone, and it
/// is no log read.
fn walk_the_reads(pic: &PocketIc, quote_hash: Hash32, latest: u64, logs: &[Value]) -> Vec<u64> {
    let mut provider = Provider::at(latest, logs).for_quote(quote_hash);
    provider.drive(pic);
    provider.froms()
}

/// A depth of six on Base, so a test can put a deposit in each of the two newest windows
/// and one of them short of the depth.
///
/// Rewritten for fix wave 6 (M1): the lookback stays at its default, forty windows; a
/// lookback of two windows is shorter than the claims the door admits, and is refused.
fn six_deep(pic: &PocketIc, canister: Principal, admin: Principal) {
    use crate::client::settlement::{get_config_full, set_config};
    let config = get_config_full(pic, canister, admin).expect("the controller reads it");
    set_config(
        pic,
        canister,
        admin,
        &Config {
            confirmations: BTreeMap::from([(1, 12), (BASE, 6)]),
            ..config
        },
    )
    .expect("the controller sets it");
}

/// The `tx_ref` the claim's `FundsReceived` recorded for `quote_hash`: which deposit it took.
fn claimed_tx_ref(pic: &PocketIc, canister: Principal, quote_hash: Hash32) -> String {
    events(pic, canister)
        .into_iter()
        .find_map(|event| match event.payload {
            EventType::FundsReceived {
                quote_hash: claimed,
                tx_ref,
                ..
            } if claimed == quote_hash => Some(tx_ref),
            _ => None,
        })
        .expect("the claim recorded the swap")
}

/// The deposit that counts is the oldest one in the whole range that is deep enough, and a
/// matching deposit that is not deep yet in a newer window never masks it: the claim reads
/// the range window by window from the oldest and takes the user's deep deposit in the
/// older window, although another deposit of the same quote and amount sits in the newer
/// one short of the depth.
///
/// Rewritten for fix wave 5 (N11): the two windows now go out in one batch, so both are
/// read, oldest first, and the older window's deep deposit is still the one taken.
/// Rewritten for fix wave 6 (M1): the two windows are the newest two of the default
/// forty, in the last batch, and every window before them is read first.
/// Rewritten for the hardening pass (H1): a deposit counts only from the quote's paying
/// wallet, its refund address, so every deposit here is from that wallet, told apart by
/// its transaction. The vault marks a quote once per payer and would not take two, but
/// the canister does not rely on the vault's mark, so the walk's order is still pinned.
#[test]
fn a_deep_deposit_in_an_older_window_claims_behind_a_shallow_one_in_a_newer() {
    let (pic, canister, admin) = setup();
    six_deep(&pic, canister, admin);
    let quote = quote(40);
    let quote_hash = swap_id(&quote);
    let deep = logged_deposit(
        quote_hash,
        HEAD - 15_000,
        USDC,
        USER,
        AMOUNT.into(),
        [0x71; 32],
    );
    let shallow = logged_deposit(quote_hash, HEAD - 2, USDC, USER, AMOUNT.into(), [0x72; 32]);
    // the controller's door, so no registration height narrows the read to one window
    let call = submit_claim_unregistered(&pic, canister, admin, &wire(&quote));
    let reads = walk_the_reads(&pic, quote_hash, HEAD, &[deep, shallow]);
    assert_eq!(await_claim(&pic, call), Ok(quote_hash));
    assert_eq!(
        claimed_tx_ref(&pic, canister, quote_hash),
        format!("0x{}", hex::encode([0x71; 32])),
        "the deep deposit in the older window"
    );
    assert_eq!(
        reads,
        (0..40)
            .map(|window| HEAD + 1 - 400_000 + window * 10_000)
            .collect::<Vec<u64>>(),
        "every window oldest first, the two holding the deposits last, in one batch"
    );
    assert_eq!(
        reads[38..],
        [HEAD + 1 - 20_000, HEAD + 1 - 10_000],
        "the deep deposit's window, then the shallow one's"
    );
    assert!(verify_replay(&pic, canister, Principal::anonymous()));
}

/// Among several deep deposits in range the claim takes the oldest, the first the quote's
/// hash was paid with, wherever the window boundaries fall: across two windows the older
/// window's, and inside one window the lower block's, whatever order the provider lists
/// them in.
///
/// Rewritten for the hardening pass (H1): a deposit counts only from the quote's paying
/// wallet, its refund address, so every deposit here is from that wallet, told apart by
/// its transaction. The vault marks a quote once per payer and would not take two, but
/// the canister does not rely on the vault's mark, so the walk's order is still pinned.
#[test]
fn the_oldest_of_several_deep_deposits_is_the_one_claimed() {
    let (pic, canister, admin) = setup();
    six_deep(&pic, canister, admin);
    let across = quote(41);
    let across_hash = swap_id(&across);
    let older = logged_deposit(
        across_hash,
        HEAD - 15_000,
        USDC,
        USER,
        AMOUNT.into(),
        [0x71; 32],
    );
    let newer = logged_deposit(
        across_hash,
        HEAD - 5_000,
        USDC,
        USER,
        AMOUNT.into(),
        [0x72; 32],
    );
    let call = submit_claim_unregistered(&pic, canister, admin, &wire(&across));
    walk_the_reads(&pic, across_hash, HEAD, &[older, newer]);
    assert_eq!(await_claim(&pic, call), Ok(across_hash));
    assert_eq!(
        claimed_tx_ref(&pic, canister, across_hash),
        format!("0x{}", hex::encode([0x71; 32])),
        "the older window's deposit, not the newer window's"
    );

    let within = quote(42);
    let within_hash = swap_id(&within);
    let later = logged_deposit(
        within_hash,
        HEAD - 8_000,
        USDC,
        USER,
        AMOUNT.into(),
        [0x73; 32],
    );
    let earlier = logged_deposit(
        within_hash,
        HEAD - 9_000,
        USDC,
        USER,
        AMOUNT.into(),
        [0x74; 32],
    );
    let call = submit_claim_unregistered(&pic, canister, admin, &wire(&within));
    walk_the_reads(&pic, within_hash, HEAD, &[later, earlier]);
    assert_eq!(await_claim(&pic, call), Ok(within_hash));
    assert_eq!(
        claimed_tx_ref(&pic, canister, within_hash),
        format!("0x{}", hex::encode([0x74; 32])),
        "the lower block's deposit, although the provider listed it second"
    );
    assert!(verify_replay(&pic, canister, Principal::anonymous()));
}

/// A depth decides money on both sides, so a chain the deploy gave no depth decides
/// nothing: the claim is refused by name before any outcall. And mainnet's depth is held
/// to the floor a reorg there makes necessary, at the config's own door.
#[test]
fn a_chain_the_config_gives_no_depth_decides_nothing() {
    use crate::client::settlement::{get_config_full, set_config};
    use settlement_api::types::config::ConfigError;
    use settlement_api::types::errors::SetConfigError;
    let (pic, canister, admin) = setup();
    let quote = quote(34);
    let quote_hash = swap_id(&quote);
    registered(&pic, canister, &quote);

    let mut depthless = get_config_full(&pic, canister, admin).expect("the controller reads it");
    depthless.confirmations = BTreeMap::new();
    set_config(&pic, canister, admin, &depthless).expect("a config may list no depths");
    assert_eq!(
        claim_swap(&pic, canister, watcher(), &wire(&quote)),
        Err(ClaimError::Deposit(DepositError::NoDepth {
            chain_id: BASE
        }))
    );
    assert!(
        pic.get_canister_http().is_empty(),
        "refused before any outcall"
    );
    assert_eq!(
        get_swap(&pic, canister, Principal::anonymous(), quote_hash),
        None
    );

    let shallow = Config {
        confirmations: BTreeMap::from([(1, 11)]),
        ..depthless.clone()
    };
    assert_eq!(
        set_config(&pic, canister, admin, &shallow),
        Err(SetConfigError::InvalidConfig(
            ConfigError::DepthTooShallow {
                chain_id: 1,
                depth: 11,
                floor: 12,
            }
        )),
        "mainnet is held to the floor at the door"
    );
    let deep_enough = Config {
        confirmations: BTreeMap::from([(1, 12), (BASE, 1)]),
        ..depthless
    };
    set_config(&pic, canister, admin, &deep_enough).expect("the floor itself is allowed");
    let call = submit_claim_unregistered(&pic, canister, watcher(), &wire(&quote));
    answer(
        &pic,
        &the_read(&pic, quote_hash),
        HEAD,
        vec![deposit_log(quote_hash, HEAD, USDC, USER, AMOUNT.into())],
    );
    assert_eq!(await_claim(&pic, call), Ok(quote_hash));
}

/// Pushes the watcher's reading of Base at `block`, fresh as of now.
fn push_head(pic: &PocketIc, canister: Principal, block: u64) {
    push_chain_data(
        pic,
        canister,
        watcher(),
        BASE,
        &ChainData {
            block,
            base_fee_wei_per_gas: Nat::from(1_000_000_000_u64),
            priority_fee_wei_per_gas: Nat::from(100_000_000_u64),
        },
    )
    .expect("the watcher may push");
}

/// A lookback of `blocks` on Base, at the depths the config already holds.
fn lookback_of(pic: &PocketIc, canister: Principal, admin: Principal, blocks: u32) {
    use crate::client::settlement::{get_config_full, set_config};
    let config = get_config_full(pic, canister, admin).expect("the controller reads it");
    set_config(
        pic,
        canister,
        admin,
        &Config {
            deposit_lookback_blocks: blocks,
            ..config
        },
    )
    .expect("the controller sets it");
}

/// A reading older than `chain_data_max_age` is no height at all, by the rule every money
/// decision on the cache takes: a quote registered behind it holds none, so its claim reads
/// the plain lookback from its oldest window, and the deposit is claimed.
///
/// Rewritten for fix wave 6 (M1): the plain lookback is the default forty windows, where
/// the test set it to two, which is shorter than the claims the door admits.
#[test]
fn a_stale_reading_registers_no_height_and_the_quote_stays_claimable() {
    let (pic, canister, _admin) = setup();
    // the reading the setup pushed ages past the ten seconds the config allows
    pic.advance_time(Duration::from_secs(11));
    let quote = quote(43);
    let quote_hash = registered(&pic, canister, &quote);
    // the claim anchors on a fresh reading, as every claim does
    push_head(&pic, canister, HEAD + 1_000);
    let call = submit_claim_unregistered(&pic, canister, watcher(), &wire(&quote));
    let reads = walk_the_reads(
        &pic,
        quote_hash,
        HEAD + 1_000,
        &[deposit_log(
            quote_hash,
            HEAD + 500,
            USDC,
            USER,
            AMOUNT.into(),
        )],
    );
    assert_eq!(await_claim(&pic, call), Ok(quote_hash));
    assert_eq!(
        reads,
        (0..40)
            .map(|window| HEAD + 1_000 + 1 - 400_000 + window * 10_000)
            .collect::<Vec<u64>>(),
        "no height, so the plain lookback from its oldest window, and not the stale head"
    );
}

/// The registration height is the watcher's head, and a watcher's head can run ahead of
/// the chain. A quote registered while such a head is fresh keeps it, and the user's
/// deposit lands below it; by the claim the chain has caught up past it, so nothing about
/// the claim's own reading looks wrong. The claim reads from the height, finds nothing at
/// all above it, and reads the plain lookback before refusing, so it claims the deposit
/// rather than stranding it.
///
/// Rewritten for fix wave 5 (L4, N-d): the read starts the margin below the height, and
/// the fallback reads only the rest of the lookback, older than that.
/// Rewritten for fix wave 6 (M1): the lookback is the default 400,000 blocks, where the
/// test set it to 100,000, which is shorter than the claims the door admits.
#[test]
fn a_watcher_head_ahead_of_the_chain_cannot_strand_a_quote() {
    let (pic, canister, _admin) = setup();
    push_head(&pic, canister, HEAD + 50_000);
    let quote = quote(44);
    let quote_hash = registered(&pic, canister, &quote);
    let deposit = deposit_log(quote_hash, HEAD + 10, USDC, USER, AMOUNT.into());
    push_head(&pic, canister, HEAD + 60_000);
    let call = submit_claim_unregistered(&pic, canister, watcher(), &wire(&quote));
    let reads = walk_the_reads(&pic, quote_hash, HEAD + 60_000, &[deposit]);
    assert_eq!(await_claim(&pic, call), Ok(quote_hash));
    assert_eq!(
        (reads[0], reads[4]),
        (HEAD + 30_000, HEAD + 60_000 + 1 - 400_000),
        "from the margin below the height the quote was registered at, then, with nothing \
         above it, the rest of the lookback from its oldest window"
    );
    assert!(verify_replay(&pic, canister, Principal::anonymous()));
}

/// Rule A5 at the door that moves money: a pull is refused for a quote its claim would
/// refuse by the quote alone, because a pull the claim then refuses leaves the user's
/// funds in the vault under a quote that never becomes a swap. A source token that is not
/// the rail's USDC, and a rail the deploy has off, are both refused before anything is
/// allocated or signed.
///
/// Rewritten for fix wave 5 (N8): the store no longer takes an Eco quote while the rail is
/// off, so the Eco quote registers with the rail on and the deploy turns it off before the
/// pull, which is the pull's own refusal this test is for.
/// Rewritten for the hardening pass (H4): the store no longer takes a quote naming a token
/// that is not the rail's either, so the quote registers under the table and the deploy
/// moves the table before the pull, which is the pull's own refusal this test is for.
#[test]
fn a_pull_for_a_quote_its_claim_would_refuse_moves_nothing() {
    use crate::client::settlement::{get_config_full, set_config};
    use settlement_api::types::quote::{QuoteAddressField, RailTokenError};
    let (pic, canister, admin) = setup_with_keys();
    let worthless = "0x2222222222222222222222222222222222222222";
    let rail_tokens_quote = types::Quote {
        gas_mode: GasMode::Gasless,
        ..live_quote(&pic, 46)
    };
    let quote_hash = registered(&pic, canister, &rail_tokens_quote);
    let table = get_config_full(&pic, canister, admin).expect("the controller reads it");
    let moved = Config {
        usdc_addresses: BTreeMap::from([
            (BASE, worthless.to_string()),
            (ARBITRUM, USDC.to_string()),
        ]),
        ..table.clone()
    };
    set_config(&pic, canister, admin, &moved).expect("the controller moves the table");
    assert_eq!(
        start_gasless_pull(
            &pic,
            canister,
            quoter(),
            quote_hash,
            &permit(&rail_tokens_quote)
        ),
        Err(PullError::RailToken(RailTokenError::NotTheRailToken {
            field: QuoteAddressField::SrcToken,
            quoted: USDC.to_string(),
            rail_token: worthless.to_string(),
            rail: "cctp_v2_fast".to_string(),
        }))
    );
    set_config(&pic, canister, admin, &table).expect("and back");

    let eco = types::Quote {
        gas_mode: GasMode::Gasless,
        rail: Rail::Eco,
        ..live_quote(&pic, 47)
    };
    let eco_hash = with_eco_on(&pic, canister, admin, || registered(&pic, canister, &eco));
    assert_eq!(
        start_gasless_pull(&pic, canister, quoter(), eco_hash, &permit(&eco)),
        Err(PullError::RailUnavailable {
            rail: "eco".to_string()
        }),
        "the rail is off on a deploy"
    );
    assert!(
        events(&pic, canister)
            .iter()
            .all(|event| !matches!(event.payload, EventType::TxCreated { .. })),
        "nothing was allocated"
    );
    assert!(pic.get_canister_http().is_empty(), "and nothing sent");
}

/// Runs `register` with the Eco rail on, and turns it off again after: the store takes an
/// Eco quote only while the deploy runs the rail.
fn with_eco_on<T>(
    pic: &PocketIc,
    canister: Principal,
    admin: Principal,
    register: impl FnOnce() -> T,
) -> T {
    use crate::client::settlement::{get_config_full, set_config};
    let off = get_config_full(pic, canister, admin).expect("the controller reads it");
    let on = Config {
        eco_enabled: true,
        ..off.clone()
    };
    set_config(pic, canister, admin, &on).expect("the controller turns the rail on");
    let registered = register();
    set_config(pic, canister, admin, &off).expect("and off again");
    registered
}

/// A quote pays its user at `dst_address`, after the burn and the mint, so text no payout
/// could be sent to is refused at both doors before anything is read or stored (rule C5),
/// exactly as a refund address that is not one is. Every chain this canister pays is an
/// EVM chain, so the destination must be an EVM address: plain text, an address missing
/// its `0x`, and an address whose mixed case breaks its EIP-55 checksum are each refused
/// with the reason, and an address still registers and claims.
#[test]
fn a_quote_naming_no_payable_destination_is_refused_at_both_doors() {
    use settlement_api::types::errors::RegisterQuoteError;
    use settlement_api::types::events::EvmAddressError;
    use settlement_api::types::quote::{QuoteAddressError, QuoteAddressField};
    let (pic, canister, _admin) = setup();
    let before = count(&pic, canister);
    for (nonce, text, reason) in [
        (50, "hello", EvmAddressError::NoPrefix),
        (
            51,
            "7551A66653f9a20979ed81835a0b7008EC83401b",
            EvmAddressError::NoPrefix,
        ),
        (
            52,
            "0x7551a66653f9a20979ed81835a0b7008EC83401b",
            EvmAddressError::BadChecksum,
        ),
    ] {
        let quote = types::Quote {
            dst_address: text.parse().unwrap(),
            ..live_quote(&pic, nonce)
        };
        assert_eq!(
            register_quote(&pic, canister, quoter(), &wire(&quote)),
            Err(RegisterQuoteError::DstAddressNotAnAddress {
                reason: reason.clone()
            }),
            "registering {text}"
        );
        assert_eq!(
            claim_swap(&pic, canister, watcher(), &wire(&quote)),
            Err(ClaimError::QuoteAddress(QuoteAddressError::NotAnAddress {
                field: QuoteAddressField::DstAddress,
                reason,
            })),
            "claiming {text}"
        );
    }
    assert!(
        pic.get_canister_http().is_empty(),
        "refused before any outcall"
    );
    assert_eq!(count(&pic, canister), before, "nothing stored");

    let good = live_quote(&pic, 53);
    let good_hash = registered(&pic, canister, &good);
    let call = submit_claim_unregistered(&pic, canister, watcher(), &wire(&good));
    answer(
        &pic,
        &the_read(&pic, good_hash),
        HEAD,
        vec![deposit_log(good_hash, HEAD, USDC, USER, AMOUNT.into())],
    );
    assert_eq!(await_claim(&pic, call), Ok(good_hash));
}

/// Walks the clock from where it stands to `until_s`, a minute at a time, the way the
/// expiry sweep's timer sees it (rule G4): it runs every minute, so a quote it would drop
/// too early is gone by the end of the walk.
fn walk_the_clock_to(pic: &PocketIc, until_s: u64) {
    loop {
        let now_s = pic.get_time().as_nanos_since_unix_epoch() / 1_000_000_000;
        if now_s >= until_s {
            return;
        }
        pic.advance_time(Duration::from_secs((until_s - now_s).min(60)));
        pic.tick();
        pic.tick();
    }
}

/// The user's deposit for `quote_hash` at `block`, in the block `block_hash` names.
fn deposit_in_block(quote_hash: Hash32, block: u64, block_hash: Hash32) -> Value {
    let mut log = deposit_log(quote_hash, block, USDC, USER, AMOUNT.into());
    log["blockHash"] = json!(format!("0x{}", hex::encode(block_hash)));
    log
}

/// The claim is judged by the deposit's own time, not by when it is asked: a deposit whose
/// block the chain made inside the quote's window is claimed ten minutes after the window
/// closed, the way a watcher that was down would ask for it. The pending store kept the
/// quote through the minutes between, and the claim read the time of the block the
/// deposit's log named, one outcall more, because its read came back past the window.
#[test]
fn a_deposit_claimed_after_its_window_closed_is_admitted_when_it_landed_in_time() {
    let (pic, canister, _admin) = setup();
    let quote = quote(60);
    let quote_hash = registered(&pic, canister, &quote);
    let block_hash = [0x51; 32];
    // the window closes at the expiry plus the two minute permit window
    walk_the_clock_to(&pic, EXPIRES_AT + 120 + 600);
    assert!(
        get_pending(&pic, canister, watcher(), quote_hash)
            .unwrap()
            .is_some(),
        "the store kept the quote past its permit window, inside the grace"
    );
    push_head(&pic, canister, HEAD + 400);
    let before = count(&pic, canister);
    let call = submit_claim_unregistered(&pic, canister, watcher(), &wire(&quote));
    let mut provider = Provider::at(
        HEAD + 400,
        &[deposit_in_block(quote_hash, HEAD + 5, block_hash)],
    )
    .for_quote(quote_hash)
    .block(block_hash, HEAD + 5, EXPIRES_AT + 60);
    provider.drive(&pic);
    assert_eq!(await_claim(&pic, call), Ok(quote_hash));
    assert_eq!(count(&pic, canister), before + 1, "the swap");
    assert_eq!(
        provider.block_reads,
        vec![format!("0x{}", hex::encode(block_hash))],
        "the time the deposit's own block was made is what it was judged by"
    );
    assert!(verify_replay(&pic, canister, Principal::anonymous()));
}

/// A deposit whose block the chain made after the quote's window closed is not the swap the
/// user was quoted, whoever asks: the watcher's claim and the controller's are both refused
/// with the block, the time it landed and the deadline it missed, and nothing is stored.
/// Its funds stay in the vault, for the late-arrival policy a later plan builds.
#[test]
fn a_deposit_that_landed_late_is_refused_whoever_asks() {
    let (pic, canister, admin) = setup();
    let quote = quote(61);
    let quote_hash = registered(&pic, canister, &quote);
    let block_hash = [0x52; 32];
    walk_the_clock_to(&pic, EXPIRES_AT + 300);
    let before = count(&pic, canister);
    for who in [watcher(), admin] {
        push_head(&pic, canister, HEAD + 400);
        let call = submit_claim_unregistered(&pic, canister, who, &wire(&quote));
        let mut provider = Provider::at(
            HEAD + 400,
            &[deposit_in_block(quote_hash, HEAD + 70, block_hash)],
        )
        .for_quote(quote_hash)
        .block(block_hash, HEAD + 70, EXPIRES_AT + 121);
        provider.drive(&pic);
        assert_eq!(
            await_claim(&pic, call),
            Err(ClaimError::LandedLate {
                block: HEAD + 70,
                landed_at_s: EXPIRES_AT + 121,
                deposit_until_s: EXPIRES_AT + 120,
            }),
            "claimed by {who}"
        );
    }
    assert_eq!(count(&pic, canister), before, "nothing stored");
    assert_eq!(
        get_swap(&pic, canister, Principal::anonymous(), quote_hash),
        None
    );
}

/// The permit window and the claim's grace, as a controller sets them, at the config's
/// other knobs as they stand.
fn claim_window_of(
    pic: &PocketIc,
    canister: Principal,
    admin: Principal,
    permit_s: u64,
    grace_s: u64,
) {
    use crate::client::settlement::{get_config_full, set_config};
    let config = get_config_full(pic, canister, admin).expect("the controller reads it");
    set_config(
        pic,
        canister,
        admin,
        &Config {
            permit_deadline_s: permit_s,
            claim_grace_s: grace_s,
            ..config
        },
    )
    .expect("the controller sets it");
}

/// A quote keeps the deadlines it was registered under (rule A3, review 5 L5): the permit
/// window and the grace are read from the config once, when the quoter registers the
/// quote, and an operator who shortens both afterwards moves neither deadline of a quote
/// already handed to a user. The claim below comes after the shortened grace has ended,
/// and its deposit landed after the shortened permit window closed; both are inside the
/// windows the quote was registered under, so the deposit is claimed.
#[test]
fn a_config_change_after_registration_moves_neither_deadline() {
    let (pic, canister, admin) = setup();
    let quote = quote(64);
    // registered under the defaults: a two minute permit window and an hour of grace
    let quote_hash = registered(&pic, canister, &quote);
    claim_window_of(&pic, canister, admin, 60, 600);
    let block_hash = [0x54; 32];
    // past the shortened claim deadline, the expiry plus 60 and 600 seconds
    walk_the_clock_to(&pic, EXPIRES_AT + 60 + 600 + 300);
    assert!(
        get_pending(&pic, canister, watcher(), quote_hash)
            .unwrap()
            .is_some(),
        "the store keeps the quote to the claim deadline it was registered under"
    );
    push_head(&pic, canister, HEAD + 400);
    let before = count(&pic, canister);
    let call = submit_claim_unregistered(&pic, canister, watcher(), &wire(&quote));
    let mut provider = Provider::at(
        HEAD + 400,
        &[deposit_in_block(quote_hash, HEAD + 5, block_hash)],
    )
    .for_quote(quote_hash)
    // after the shortened permit window, inside the one the quote was registered under
    .block(block_hash, HEAD + 5, EXPIRES_AT + 100);
    provider.drive(&pic);
    assert_eq!(await_claim(&pic, call), Ok(quote_hash));
    assert_eq!(count(&pic, canister), before + 1, "the swap");
    assert!(verify_replay(&pic, canister, Principal::anonymous()));
}

/// The controller's claim, the ops path for a deposit the services never claimed, is held
/// to the same window as theirs: a deposit that landed in time is claimed by hand after
/// the window closed, and once the grace has ended the controller is refused like anyone
/// else, before any outcall.
#[test]
fn a_controllers_claim_is_held_to_the_same_window() {
    let (pic, canister, admin) = setup();
    // never registered: the controller's own door
    let by_hand = quote(62);
    let by_hand_hash = swap_id(&by_hand);
    let block_hash = [0x53; 32];
    walk_the_clock_to(&pic, EXPIRES_AT + 120 + 600);
    push_head(&pic, canister, HEAD + 400);
    let call = submit_claim_unregistered(&pic, canister, admin, &wire(&by_hand));
    let mut provider = Provider::at(
        HEAD + 400,
        &[deposit_in_block(by_hand_hash, HEAD + 5, block_hash)],
    )
    .for_quote(by_hand_hash)
    .block(block_hash, HEAD + 5, EXPIRES_AT + 100);
    provider.drive(&pic);
    assert_eq!(await_claim(&pic, call), Ok(by_hand_hash));

    // past the grace nobody is admitted, the controller included
    walk_the_clock_to(&pic, EXPIRES_AT + 120 + 3_600 + 1);
    push_head(&pic, canister, HEAD + 400);
    let refused = claim_swap(&pic, canister, admin, &wire(&quote(63)));
    assert!(
        matches!(
            refused,
            Err(ClaimError::QuoteExpired { claim_until_s, .. })
                if claim_until_s == EXPIRES_AT + 120 + 3_600
        ),
        "{refused:?}"
    );
    assert!(
        pic.get_canister_http().is_empty(),
        "refused before any outcall"
    );
}

/// A watcher's head ahead of the chain cannot stall a claim on a provider that refuses a
/// range above its own head, the way geth-family providers do (review 4, M1): the claim
/// asks the provider's head first and builds every window from the lower of it and the
/// watcher's, so no range the provider refuses is sent, and the deposit below the false
/// height is claimed.
#[test]
fn a_provider_that_refuses_a_range_above_its_head_does_not_stall_the_claim() {
    let (pic, canister, _admin) = setup();
    push_head(&pic, canister, HEAD + 50_000);
    let quote = quote(64);
    let quote_hash = registered(&pic, canister, &quote);
    let call = submit_claim_unregistered(&pic, canister, watcher(), &wire(&quote));
    let mut provider = Provider::at(
        HEAD + 20,
        &[deposit_log(
            quote_hash,
            HEAD + 10,
            USDC,
            USER,
            AMOUNT.into(),
        )],
    )
    .shaped(Shape::Geth)
    .for_quote(quote_hash);
    provider.drive(&pic);
    assert_eq!(await_claim(&pic, call), Ok(quote_hash));
    assert!(
        provider
            .log_reads
            .iter()
            .all(|(from, to)| *from <= HEAD + 20 && to.is_none_or(|to| to <= HEAD + 20)),
        "no range above the provider's head was asked for: {:?}",
        provider.log_reads
    );
}

/// The same lying head against a provider that answers a range above its head with
/// nothing rather than an error: the claim still never asks for one, and the deposit below
/// the false height is claimed.
#[test]
fn a_permissive_provider_is_never_asked_for_a_range_above_its_head() {
    let (pic, canister, _admin) = setup();
    push_head(&pic, canister, HEAD + 50_000);
    let quote = quote(66);
    let quote_hash = registered(&pic, canister, &quote);
    let call = submit_claim_unregistered(&pic, canister, watcher(), &wire(&quote));
    let mut provider = Provider::at(
        HEAD + 20,
        &[deposit_log(
            quote_hash,
            HEAD + 10,
            USDC,
            USER,
            AMOUNT.into(),
        )],
    )
    .for_quote(quote_hash);
    provider.drive(&pic);
    assert_eq!(await_claim(&pic, call), Ok(quote_hash));
    assert!(
        provider
            .log_reads
            .iter()
            .all(|(from, _)| *from <= HEAD + 20),
        "no range above the provider's head was asked for: {:?}",
        provider.log_reads
    );
}

/// The height a quote was registered at is the watcher's, and a watcher can push one a
/// little ahead of the chain (review 4, L4). The user's deposit lands below it, and a
/// later deposit of the same quote and amount lands above it once the chain has passed
/// it: the user's, the oldest, is the one claimed, because the read starts a margin below
/// the height rather than at it.
///
/// Rewritten for the hardening pass (H1): a deposit counts only from the quote's paying
/// wallet, its refund address, so every deposit here is from that wallet, told apart by
/// its transaction. The vault marks a quote once per payer and would not take two, but
/// the canister does not rely on the vault's mark, so the walk's order is still pinned.
#[test]
fn a_deposit_below_a_lying_height_counts_before_a_later_one_above_it() {
    let (pic, canister, _admin) = setup();
    push_head(&pic, canister, HEAD + 1_000);
    let quote = quote(65);
    let quote_hash = registered(&pic, canister, &quote);
    let users = logged_deposit(quote_hash, HEAD + 10, USDC, USER, AMOUNT.into(), [0x71; 32]);
    let later = logged_deposit(
        quote_hash,
        HEAD + 2_000,
        USDC,
        USER,
        AMOUNT.into(),
        [0x72; 32],
    );
    push_head(&pic, canister, HEAD + 3_000);
    let call = submit_claim_unregistered(&pic, canister, watcher(), &wire(&quote));
    walk_the_reads(&pic, quote_hash, HEAD + 3_000, &[users, later]);
    assert_eq!(await_claim(&pic, call), Ok(quote_hash));
    assert_eq!(
        claimed_tx_ref(&pic, canister, quote_hash),
        format!("0x{}", hex::encode([0x71; 32])),
        "the user's deposit below the height, the oldest, and not the later one above it"
    );
}

/// The widest read, the whole default lookback of 400,000 blocks, is 40 windows, and it
/// is read in a handful of outcalls: the provider's head, then the windows ten to a
/// JSON-RPC batch, where it took one outcall a window. A controller's claim of a quote the
/// store never held reads it all when the deposit sits in the newest window.
///
/// Rewritten for fix wave 6 (M1): the default lookback was 345,600 blocks, 35 windows.
#[test]
fn the_default_lookback_is_read_in_a_handful_of_outcalls() {
    let (pic, canister, admin) = setup();
    let by_hand = quote(70);
    let quote_hash = swap_id(&by_hand);
    let call = submit_claim_unregistered(&pic, canister, admin, &wire(&by_hand));
    let mut provider = Provider::at(
        HEAD,
        &[deposit_log(quote_hash, HEAD - 3, USDC, USER, AMOUNT.into())],
    )
    .for_quote(quote_hash);
    provider.drive(&pic);
    assert_eq!(await_claim(&pic, call), Ok(quote_hash));
    assert_eq!(provider.log_reads.len(), 40, "every window of the lookback");
    assert_eq!(
        provider.outcalls, 5,
        "the head, then four batches of at most ten windows"
    );
}

/// A provider that refuses a batch of windows does not fail the read: the batch is read
/// again one window at a time, every window of it, and the deposit in the newest is
/// claimed.
///
/// Rewritten for fix wave 6 (M1): the default lookback is forty windows, where it was 35.
#[test]
fn a_batch_the_provider_refuses_is_read_one_window_at_a_time() {
    let (pic, canister, admin) = setup();
    let by_hand = quote(71);
    let quote_hash = swap_id(&by_hand);
    let call = submit_claim_unregistered(&pic, canister, admin, &wire(&by_hand));
    let mut provider = Provider::at(
        HEAD,
        &[deposit_log(quote_hash, HEAD - 3, USDC, USER, AMOUNT.into())],
    )
    .for_quote(quote_hash)
    .batches_of_at_most(1);
    provider.drive(&pic);
    assert_eq!(await_claim(&pic, call), Ok(quote_hash));
    assert_eq!(
        provider.refused_batches, 4,
        "each batch of windows was offered and refused"
    );
    assert_eq!(
        provider.log_reads.len(),
        40,
        "and every window was read on its own after it"
    );
    let mut froms = provider.froms();
    froms.dedup();
    assert_eq!(froms.len(), 40, "each window once");
}

/// Registration refuses what a claim would refuse by the quote alone (rule A5), so no quote
/// is handed to a user to pay that no claim could then take: a halted canister registers
/// nothing (no new swaps during a halt), a quote on the Eco rail while the deploy has it
/// off is refused by its rail, and the zero address as the destination or the refund
/// address, which the vault cannot pay, is refused by the field at both doors. Nothing is
/// stored and nothing is read.
#[test]
fn registration_refuses_what_a_claim_would_refuse() {
    use settlement_api::types::errors::RegisterQuoteError;
    use settlement_api::types::quote::{QuoteAddressError, QuoteAddressField};
    let (pic, canister, admin) = setup();

    set_halted(&pic, canister, admin, true).expect("the operator halts");
    let during = quote(80);
    assert_eq!(
        register_quote(&pic, canister, quoter(), &wire(&during)),
        Err(RegisterQuoteError::Guard(GuardError::Halted)),
        "no new swaps during a halt"
    );
    set_halted(&pic, canister, admin, false).expect("the operator lifts the halt");
    assert_eq!(
        get_pending(&pic, canister, watcher(), swap_id(&during)).unwrap(),
        None,
        "nothing was registered during the halt"
    );

    let before = count(&pic, canister);
    let eco = types::Quote {
        rail: Rail::Eco,
        ..quote(81)
    };
    assert_eq!(
        register_quote(&pic, canister, quoter(), &wire(&eco)),
        Err(RegisterQuoteError::RailUnavailable {
            rail: "eco".to_string()
        }),
        "the claim would refuse the rail, so the store does"
    );

    let zero: types::Address = "0x0000000000000000000000000000000000000000"
        .parse()
        .unwrap();
    for (quote, field) in [
        (
            types::Quote {
                dst_address: zero.clone(),
                ..quote(82)
            },
            QuoteAddressField::DstAddress,
        ),
        (
            types::Quote {
                refund_address: Some(zero.clone()),
                ..quote(83)
            },
            QuoteAddressField::RefundAddress,
        ),
    ] {
        assert_eq!(
            register_quote(&pic, canister, quoter(), &wire(&quote)),
            Err(RegisterQuoteError::QuoteAddress(QuoteAddressError::Zero {
                field
            })),
            "registering a zero {field:?}"
        );
        assert_eq!(
            claim_swap(&pic, canister, watcher(), &wire(&quote)),
            Err(ClaimError::QuoteAddress(QuoteAddressError::Zero { field })),
            "claiming a zero {field:?}"
        );
        assert_eq!(
            get_pending(&pic, canister, watcher(), swap_id(&quote)).unwrap(),
            None
        );
    }
    assert!(pic.get_canister_http().is_empty(), "nothing was read");
    assert_eq!(count(&pic, canister), before, "nothing stored");
}

/// Turning a rail off pauses the swaps on it and never stops them (review 4, L3): an Eco
/// swap funded while the rail ran is refused its next move on every engine tick while the
/// deploy has the rail off, stays where it was with no line appended, and is counted by the
/// world-readable `paused_swaps` query, so the pause is seen (E6). With the rail back on
/// it is no longer counted.
///
/// Rewritten for the hardening pass (H4, review 5 L6): the query answers one bounded page,
/// with how many swaps it read and the swap the next page starts after, where it answered
/// a bare count read off every swap ever folded. One swap is one page, with none after it.
#[test]
fn a_rail_turned_off_pauses_its_swaps_and_the_pause_is_counted() {
    use crate::client::settlement::{get_config_full, set_config};
    let (pic, canister, admin) = setup();
    let off = get_config_full(&pic, canister, admin).expect("the controller reads it");
    let on = Config {
        eco_enabled: true,
        ..off.clone()
    };
    set_config(&pic, canister, admin, &on).expect("the controller turns the rail on");
    let eco = types::Quote {
        rail: Rail::Eco,
        ..quote(90)
    };
    let eco_hash = swap_id(&eco);
    let call = submit_claim(&pic, canister, watcher(), &eco);
    answer(
        &pic,
        &the_read(&pic, eco_hash),
        HEAD,
        vec![deposit_log(eco_hash, HEAD, USDC, USER, AMOUNT.into())],
    );
    assert_eq!(await_claim(&pic, call), Ok(eco_hash));
    let page = |paused: u64| PausedSwapsPage {
        paused,
        read: 1,
        next: None,
    };
    assert_eq!(
        paused_swaps(&pic, canister, Principal::anonymous(), None),
        page(0)
    );

    set_config(&pic, canister, admin, &off).expect("the controller turns it off");
    let before = count(&pic, canister);
    assert_eq!(
        paused_swaps(&pic, canister, Principal::anonymous(), None),
        page(1),
        "the swap the rail holds is counted, for anyone to see"
    );
    assert_eq!(
        paused_swaps(&pic, canister, Principal::anonymous(), Some(eco_hash)),
        PausedSwapsPage {
            paused: 0,
            read: 0,
            next: None,
        },
        "and a page after the last swap reads nothing"
    );
    // a few engine ticks: the swap is refused its move and nothing is appended
    for _ in 0..3 {
        pic.advance_time(Duration::from_secs(30));
        for _ in 0..4 {
            pic.tick();
        }
    }
    assert!(pic.get_canister_http().is_empty(), "the rail read nothing");
    assert_eq!(count(&pic, canister), before, "and no line was written");
    assert_eq!(
        get_swap(&pic, canister, Principal::anonymous(), eco_hash)
            .expect("the swap exists")
            .status,
        SwapStatus::FundsReceived,
        "paused where it was, not frozen"
    );

    set_config(&pic, canister, admin, &on).expect("the controller turns it back on");
    assert_eq!(
        paused_swaps(&pic, canister, Principal::anonymous(), None),
        page(0)
    );
}

/// A batch the provider refuses is read again one window at a time, oldest first, and each
/// window's finding is taken as it comes: a deep deposit in an older window is claimed
/// although a newer window of the same batch cannot be read, just as the window by window
/// walk before batching claimed it without ever asking for the newer one. A window that
/// cannot be read decides nothing it did not reach.
///
/// Rewritten for fix wave 6 (M1): the batch is the first ten windows of the default
/// lookback, the deposit in its oldest and the block the provider cannot serve in its
/// newest, where the test set a lookback of three windows, which is shorter than the
/// claims the door admits.
#[test]
fn a_window_that_cannot_be_read_does_not_hide_an_older_deposit_in_its_batch() {
    let (pic, canister, admin) = setup();
    let by_hand = quote(72);
    let quote_hash = swap_id(&by_hand);
    let call = submit_claim_unregistered(&pic, canister, admin, &wire(&by_hand));
    let oldest = HEAD + 1 - 400_000;
    let mut provider = Provider::at(
        HEAD,
        &[deposit_log(
            quote_hash,
            oldest + 5_000,
            USDC,
            USER,
            AMOUNT.into(),
        )],
    )
    .for_quote(quote_hash)
    .refusing_block(oldest + 95_000);
    provider.drive(&pic);
    assert_eq!(await_claim(&pic, call), Ok(quote_hash));
    assert_eq!(
        provider.froms()[10..],
        [oldest],
        "after the refused batch, the oldest window alone, which holds the deposit"
    );
}

/// A deposit of one unit of the quote's own token under `quote_hash` from payer number
/// `n`, in its own transaction: dust anyone can log under a user's public quote hash, once
/// per payer (the vault marks a quote per payer), for gas and one unit. It is the quote's
/// token, so no filter on the token drops it.
fn token_dust(quote_hash: Hash32, block: u64, n: u64) -> Value {
    let payer = format!("0x{:040x}", 0x1000 + n);
    let mut tx = [0u8; 32];
    tx[..8].copy_from_slice(&n.to_be_bytes());
    tx[31] = 0xdd;
    logged_deposit(quote_hash, block, USDC, &payer, 1, tx)
}

/// A zero-value `depositNative(quote_hash)` from payer number `n`: dust for gas alone,
/// which a read that asks for the quote's token never gets back.
fn native_dust(quote_hash: Hash32, block: u64, n: u64) -> Value {
    let payer = format!("0x{:040x}", 0x9000 + n);
    let mut tx = [0u8; 32];
    tx[..8].copy_from_slice(&n.to_be_bytes());
    tx[31] = 0xee;
    logged_deposit(
        quote_hash,
        block,
        "0x0000000000000000000000000000000000000000",
        &payer,
        0,
        tx,
    )
}

/// The user's deposit at `HEAD - 3`, and `dust` deposits of one unit of the quote's token
/// from distinct payers at `block`.
fn dusted(quote_hash: Hash32, dust: u64, block: u64) -> Vec<Value> {
    let mut logs = vec![deposit_log(quote_hash, HEAD - 3, USDC, USER, AMOUNT.into())];
    logs.extend((0..dust).map(|n| token_dust(quote_hash, block, n)));
    logs
}

/// One claim of `quote` by `who` against a provider at `HEAD` holding `logs`, behind the
/// replica's size rule: what the claim answers, and the provider, for what it was asked.
fn dusty_claim(
    pic: &PocketIc,
    canister: Principal,
    who: Principal,
    quote: &types::Quote,
    logs: &[Value],
) -> (Result<Hash32, ClaimError>, Provider) {
    claim_against(pic, canister, who, quote, Provider::at(HEAD, logs))
}

/// The same against a provider deaf to the payer the read names: every payer's logs come
/// back, a stranger's dust among them, and the canister refuses them log by log.
fn deaf_dusty_claim(
    pic: &PocketIc,
    canister: Principal,
    who: Principal,
    quote: &types::Quote,
    logs: &[Value],
) -> (Result<Hash32, ClaimError>, Provider) {
    claim_against(
        pic,
        canister,
        who,
        quote,
        Provider::at(HEAD, logs).deaf_to_the_payer(),
    )
}

/// One claim of `quote` by `who` against `provider`, behind the replica's size rule.
fn claim_against(
    pic: &PocketIc,
    canister: Principal,
    who: Principal,
    quote: &types::Quote,
    provider: Provider,
) -> (Result<Hash32, ClaimError>, Provider) {
    push_head(pic, canister, HEAD);
    let call = submit_claim_unregistered(pic, canister, who, &wire(quote));
    let mut provider = provider.for_quote(swap_id(quote)).enforcing_caps();
    provider.drive(pic);
    (await_claim(pic, call), provider)
}

/// Whether every log read named the user's wallet as the payer (`topics[3]`).
fn every_read_names_the_user(provider: &Provider) -> bool {
    !provider.payer_topics.is_empty()
        && provider
            .payer_topics
            .iter()
            .all(|payer| *payer == json!(word_of(USER)))
}

/// The largest answer an outcall may be, as the system holds it: two megabytes.
const LARGEST_ANSWER: u64 = 2_000_000;

/// Dust enough that one block of it is more than [`LARGEST_ANSWER`]: about 646 bytes a
/// log as this mock prints one, so 3,300 logs are about 2.13 megabytes.
const BLOCK_OF_DUST: u64 = 3_300;

/// Ported from review 5's first proof of concept (H1, G7). Dust logs one block after the
/// user's deposit, in the same window, put that window's answer past its cap. The claim
/// asks for the window again with a larger cap until the answer fits, and the watcher's
/// first claim takes the deposit, where every claim used to fail on the read until the
/// quote expired and the deposit never became a swap. Thirty dust logs, the control, fit
/// the first cap. Native dust, which costs gas alone, never comes back at all: every read
/// asks for the quote's token.
///
/// Rewritten for the hardening pass (H1): the dust comes from other wallets, as the proof
/// of concept sent it, and every read names the user's wallet as the payer, so a node
/// hands none of it back. The deposit fits the first cap, in the head and one batch.
#[test]
fn a_deposit_behind_160_dust_logs_in_its_own_window_is_claimed() {
    let (pic, canister, _admin) = setup();

    let control = quote(900);
    let control_hash = registered(&pic, canister, &control);
    let (claimed, provider) = dusty_claim(
        &pic,
        canister,
        watcher(),
        &control,
        &dusted(control_hash, 30, HEAD - 2),
    );
    assert_eq!(claimed, Ok(control_hash), "the control is claimed");
    assert_eq!(provider.oversized(), vec![], "and fits the first cap");

    let victim = quote(901);
    let victim_hash = registered(&pic, canister, &victim);
    let mut logs = dusted(victim_hash, 160, HEAD - 2);
    logs.extend((0..160).map(|n| native_dust(victim_hash, HEAD - 2, n)));
    let before = count(&pic, canister);
    let (claimed, provider) = dusty_claim(&pic, canister, watcher(), &victim, &logs);
    println!("sizes (cap, body): {:?}", provider.sizes);
    assert_eq!(
        claimed,
        Ok(victim_hash),
        "the watcher's first claim takes it"
    );
    assert_eq!(
        provider.oversized(),
        vec![],
        "no answer was over its cap: the dust is never in it"
    );
    assert_eq!(provider.outcalls, 2, "the head and one batch");
    assert!(
        provider
            .token_topics
            .iter()
            .all(|token| *token == json!(word_of(USDC))),
        "every read asks for the quote's token alone: {:?}",
        provider.token_topics
    );
    assert!(
        every_read_names_the_user(&provider),
        "every read asks for the user's wallet alone: {:?}",
        provider.payer_topics
    );
    assert_eq!(
        count(&pic, canister),
        before + 1,
        "the swap's FundsReceived"
    );
    assert!(get_swap(&pic, canister, Principal::anonymous(), victim_hash).is_some());
}

/// Ported from review 5's second proof of concept (H1, G7). At 160 dust logs the PoC found
/// one escape: empty the pending store, raise the lookback to its ceiling and claim by
/// hand, at the cost of every other pending quote, each of which then needed a claim by
/// hand. The watcher's own claim now takes the deposit, so the store keeps the other
/// quote and the lookback is left alone; and the operator's door, over the widest
/// lookback, still reads a deposit behind the same dust for a quote the store never held.
///
/// Rewritten for the hardening pass (H1): the dust comes from other wallets and every read
/// names the user's wallet as the payer, so neither claim is handed any of it.
#[test]
fn a_deposit_behind_160_dust_logs_needs_no_operator_escape() {
    let (pic, canister, admin) = setup();
    let bystander = quote(902);
    let bystander_hash = registered(&pic, canister, &bystander);
    let victim = quote(903);
    let victim_hash = registered(&pic, canister, &victim);
    let (claimed, provider) = dusty_claim(
        &pic,
        canister,
        watcher(),
        &victim,
        &dusted(victim_hash, 160, HEAD - 2),
    );
    assert_eq!(claimed, Ok(victim_hash), "the watcher's claim takes it");
    assert_eq!(
        provider.oversized(),
        vec![],
        "and no answer was over its cap"
    );
    assert!(every_read_names_the_user(&provider));
    assert!(
        get_pending(&pic, canister, watcher(), bystander_hash)
            .unwrap()
            .is_some(),
        "and every other pending quote stays where it was"
    );

    lookback_of(&pic, canister, admin, 400_000);
    let by_hand = quote(904);
    let by_hand_hash = swap_id(&by_hand);
    let (claimed, provider) = dusty_claim(
        &pic,
        canister,
        admin,
        &by_hand,
        &dusted(by_hand_hash, 160, HEAD - 2),
    );
    assert_eq!(
        claimed,
        Ok(by_hand_hash),
        "the operator's door reads it too"
    );
    assert_eq!(
        provider.oversized(),
        vec![],
        "and no answer was over its cap"
    );
    assert!(every_read_names_the_user(&provider));
}

/// Ported from review 5's third proof of concept (H1, G7). 520 dust logs in the deposit's
/// own block are over the ten-window batch of the widest lookback and over a window's own
/// cap, which closed every path, the operator's escape included, and left the deposit to
/// become `QuoteExpired`. The window's cap now grows until the answer fits: the watcher's
/// claim takes the deposit, and so does the operator's door over the widest lookback.
///
/// Rewritten for the hardening pass (H1): the dust comes from other wallets and every read
/// names the user's wallet as the payer, so no answer holds it and no cap has to grow.
#[test]
fn a_deposit_behind_520_dust_logs_in_its_own_block_is_claimed() {
    let (pic, canister, admin) = setup();
    let victim = quote(905);
    let victim_hash = registered(&pic, canister, &victim);
    let before = count(&pic, canister);
    let (claimed, provider) = dusty_claim(
        &pic,
        canister,
        watcher(),
        &victim,
        &dusted(victim_hash, 520, HEAD - 3),
    );
    println!("sizes (cap, body): {:?}", provider.sizes);
    assert_eq!(claimed, Ok(victim_hash), "the watcher's claim takes it");
    assert_eq!(
        provider.oversized(),
        vec![],
        "and no answer was over its cap"
    );
    assert!(every_read_names_the_user(&provider));
    assert_eq!(
        count(&pic, canister),
        before + 1,
        "the swap's FundsReceived"
    );

    lookback_of(&pic, canister, admin, 400_000);
    let by_hand = quote(906);
    let by_hand_hash = swap_id(&by_hand);
    let (claimed, _) = dusty_claim(
        &pic,
        canister,
        admin,
        &by_hand,
        &dusted(by_hand_hash, 520, HEAD - 3),
    );
    assert_eq!(
        claimed,
        Ok(by_hand_hash),
        "the operator's door reads it too"
    );
}

/// A block of dust more than the largest answer an outcall may be, one block after the
/// deposit in the same window: no cap fits the window, so it is read in halves, the older
/// half first, and a half no cap fits is halved again, until the part holding the deposit
/// fits and the deposit is claimed. Every half is read at the largest cap, and every range
/// read after the first split lies inside the window.
///
/// Rewritten for the hardening pass (H1): a node that filters on the payer never hands a
/// stranger's dust over, so the dust reaches the read here only through a provider deaf to
/// the payer. The split still reads down to the deposit, and the canister refuses every
/// dust log it is handed.
#[test]
fn a_window_no_answer_can_hold_is_split_until_the_deposit_shows() {
    let (pic, canister, _admin) = setup();
    let quote = quote(907);
    let quote_hash = registered(&pic, canister, &quote);
    let (claimed, provider) = deaf_dusty_claim(
        &pic,
        canister,
        watcher(),
        &quote,
        &dusted(quote_hash, BLOCK_OF_DUST, HEAD - 2),
    );
    println!("reads: {:?}", provider.log_reads);
    println!("sizes (cap, body): {:?}", provider.sizes);
    assert_eq!(claimed, Ok(quote_hash), "the deposit is claimed");
    let first_split = provider
        .sizes
        .iter()
        .position(|(cap, len)| *cap == Some(LARGEST_ANSWER) && *len as u64 > LARGEST_ANSWER)
        .expect("the window was refused at the largest cap");
    let halves = &provider.log_reads[provider.answered[first_split]..];
    let window_from = HEAD + 1 - 10_000;
    assert_eq!(
        halves[0],
        (window_from, Some(window_from + (HEAD - window_from) / 2)),
        "the older half of the window first"
    );
    assert!(
        halves
            .iter()
            .all(|(from, to)| *from >= window_from && to.is_none_or(|to| to <= HEAD)),
        "every half inside the window"
    );
    assert!(
        provider.sizes[first_split + 1..]
            .iter()
            .all(|(cap, _)| *cap == Some(LARGEST_ANSWER)),
        "and every half read at the largest cap"
    );
    let (from, to) = *halves.last().unwrap();
    assert!(
        from <= HEAD - 3 && to == Some(HEAD - 3),
        "the last read ends at the deposit's block, short of the dust: {from}..{to:?}"
    );
}

/// A single block whose dust alone is more than the largest answer an outcall may be
/// cannot be read by any outcall. The claim reads its window down to that one block and
/// refuses by name, with the block and the cap, rather than as a transport failure: the
/// watcher can tell it from a provider that is down, and it never stalls in silence.
/// Nothing is stored.
///
/// Rewritten for the hardening pass (H1): a node that filters on the payer never hands a
/// stranger's dust over, so a block of it reaches the read here only through a provider
/// deaf to the payer, the one shape left in which a block no answer can hold is met.
#[test]
fn a_block_no_answer_can_hold_is_refused_by_name() {
    let (pic, canister, _admin) = setup();
    let quote = quote(908);
    let quote_hash = registered(&pic, canister, &quote);
    let before = count(&pic, canister);
    let (claimed, provider) = deaf_dusty_claim(
        &pic,
        canister,
        watcher(),
        &quote,
        &dusted(quote_hash, BLOCK_OF_DUST, HEAD - 3),
    );
    println!("reads: {:?}", provider.log_reads);
    assert_eq!(
        claimed,
        Err(ClaimError::Deposit(DepositError::BlockTooLarge {
            block: HEAD - 3,
            cap: LARGEST_ANSWER,
        }))
    );
    assert_eq!(count(&pic, canister), before, "nothing stored");
    assert!(
        provider.outcalls <= 49,
        "inside the bound the in-flight marker is sized from: {}",
        provider.outcalls
    );
}

/// A block no answer can hold, above the height the quote was registered at, does not
/// hide the user's deposit below that height: the rest of the lookback, every block of
/// it older than the one that cannot be read, is read before the claim refuses, and the
/// deposit there, the oldest in range, is claimed.
///
/// Rewritten for the hardening pass (H1): the dust reaches the read only through a
/// provider deaf to the payer, as in the two tests above.
#[test]
fn a_block_no_answer_can_hold_does_not_hide_an_older_deposit() {
    let (pic, canister, _admin) = setup();
    // a watcher's head ahead of the chain, which the quote is registered at
    push_head(&pic, canister, HEAD + 50_000);
    let quote = quote(909);
    let quote_hash = registered(&pic, canister, &quote);
    let mut logs = vec![deposit_log(
        quote_hash,
        HEAD + 10,
        USDC,
        USER,
        AMOUNT.into(),
    )];
    logs.extend((0..BLOCK_OF_DUST).map(|n| token_dust(quote_hash, HEAD + 59_000, n)));
    push_head(&pic, canister, HEAD + 60_000);
    let call = submit_claim_unregistered(&pic, canister, watcher(), &wire(&quote));
    let mut provider = Provider::at(HEAD + 60_000, &logs)
        .deaf_to_the_payer()
        .for_quote(quote_hash)
        .enforcing_caps();
    provider.drive(&pic);
    println!("reads: {:?}", provider.log_reads);
    assert_eq!(await_claim(&pic, call), Ok(quote_hash));
    assert!(
        provider.outcalls <= 49,
        "inside the bound the in-flight marker is sized from: {}",
        provider.outcalls
    );
}

/// A deposit counts only from the wallet the quote names: on an EVM source chain the
/// quote's refund address is the paying wallet. A deposit of the quote's token, in exactly
/// its amount, from any other wallet is not the quote's deposit: every read names the
/// user's wallet as the payer, so a node never hands the stranger's deposit over and the
/// claim finds none. A provider deaf to the payer hands it over anyway, and the canister
/// refuses it itself. Nothing is stored either way.
#[test]
fn a_deposit_from_another_wallet_is_not_the_quotes_deposit() {
    let (pic, canister, _admin) = setup();
    let quote = quote(910);
    let quote_hash = registered(&pic, canister, &quote);
    let before = count(&pic, canister);
    let theirs = deposit_log(quote_hash, HEAD - 3, USDC, STRANGER, AMOUNT.into());

    let (claimed, provider) = dusty_claim(
        &pic,
        canister,
        watcher(),
        &quote,
        std::slice::from_ref(&theirs),
    );
    assert_eq!(
        claimed,
        Err(ClaimError::Deposit(DepositError::NotFound { quote_hash })),
        "a node filters on the payer, so the stranger's deposit never comes back"
    );
    assert!(
        every_read_names_the_user(&provider),
        "every read asks for the user's wallet alone: {:?}",
        provider.payer_topics
    );

    let (claimed, _) = deaf_dusty_claim(&pic, canister, watcher(), &quote, &[theirs]);
    assert_eq!(
        claimed,
        Err(ClaimError::Deposit(DepositError::NoneMatches {
            quote_hash,
            seen: 1
        })),
        "handed over anyway, it is refused by the canister itself"
    );
    assert_eq!(count(&pic, canister), before, "nothing stored");
    assert_eq!(
        get_swap(&pic, canister, Principal::anonymous(), quote_hash),
        None
    );
}
