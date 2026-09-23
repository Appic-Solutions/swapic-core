//! The engine from outside: on its timer it sends the rail's first leg for a claimed swap,
//! one leg at a time, and stops a swap no leg leads from.

use crate::client::settlement::{
    append, derive_evm_address, events_page, get_swap, push_chain_data, set_halted, verify_replay,
};
use crate::settlement_suite::init::{install, quoter, watcher};
use candid::{Nat, Principal};
use pocket_ic::common::rest::{
    CanisterHttpReply, CanisterHttpRequest, CanisterHttpResponse, MockCanisterHttpResponse,
};
use pocket_ic::{PocketIc, PocketIcBuilder};
use serde_json::{json, Value};
use settlement_api::types::chain_data::ChainData;
use settlement_api::types::config::Config;
use settlement_api::types::events::{Event, EventType, Hash32, TxPurpose};
use settlement_api::types::init::InitArg;
use settlement_api::types::swap::{Leg, SwapStatus};
use std::collections::BTreeMap;
use std::time::Duration;
use types::abi::{decode_cctp_deposit_for_burn_with_hook, decode_vault_execute};
use types::{ChainId, GasMode, Rail, TokenAmount, UnixSeconds};

pub const BASE: u64 = 8453;
pub const ARBITRUM: u64 = 42161;
pub const VAULT_BASE: &str = "0x1111111111111111111111111111111111111111";
pub const VAULT_ARBITRUM: &str = "0x2222222222222222222222222222222222222222";
pub const USDC_BASE: &str = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913";
pub const USDC_ARBITRUM: &str = "0xaf88d065e77c8cC2239327C5EDb3A432268e5831";
pub const TOKEN_MESSENGER: &str = "0x28b5a0e9C621a5BadaA536219b3a228C8168cf5d";
pub const MESSAGE_TRANSMITTER: &str = "0x81D40F21F12A8F0E3252Bccb954D722d4c464B64";
pub const USER: &str = "0x7551A66653f9a20979ed81835a0b7008EC83401b";
pub const AMOUNT: u32 = 25_000_000;

/// The engine's tick: the default `rail_status_max_age`.
pub const TICK: Duration = Duration::from_secs(30);

/// A Base to Arbitrum USDC quote on the fast CCTP rail.
pub fn quote(nonce: u64) -> types::Quote {
    types::Quote {
        version: 1,
        src_chain: ChainId::BASE,
        src_token: USDC_BASE.parse().unwrap(),
        amount_in: TokenAmount::from(AMOUNT),
        dst_chain: ChainId::ARBITRUM,
        dst_token: USDC_ARBITRUM.parse().unwrap(),
        expected_out: TokenAmount::from(24_990_000_u32),
        min_out: TokenAmount::from(24_900_000_u32),
        dst_address: USER.parse().unwrap(),
        refund_address: Some(USER.parse().unwrap()),
        auto_refund: true,
        gas_mode: GasMode::Legacy,
        rail: Rail::CctpV2Fast,
        expires_at: UnixSeconds::new(1_800_000_000),
        nonce,
    }
}

pub fn swap_id(quote: &types::Quote) -> Hash32 {
    quote.hash().expect("a valid quote has an id").into_bytes()
}

/// Every rail knob set for Base and Arbitrum, with a provider for each.
pub fn config() -> Config {
    Config {
        rpc_urls: BTreeMap::from([
            (BASE, "https://base-mainnet.example/v2/key".to_string()),
            (ARBITRUM, "https://arb-mainnet.example/v2/key".to_string()),
        ]),
        vault_addresses: BTreeMap::from([
            (BASE, VAULT_BASE.to_string()),
            (ARBITRUM, VAULT_ARBITRUM.to_string()),
        ]),
        cctp_domains: BTreeMap::from([(BASE, 6), (ARBITRUM, 3)]),
        usdc_addresses: BTreeMap::from([
            (BASE, USDC_BASE.to_string()),
            (ARBITRUM, USDC_ARBITRUM.to_string()),
        ]),
        token_messenger: Some(TOKEN_MESSENGER.to_string()),
        message_transmitter: Some(MESSAGE_TRANSMITTER.to_string()),
        ..Config::default()
    }
}

/// A canister on a network holding the test threshold keys, with every rail knob set, its
/// address derived, and fresh readings for both chains.
pub fn setup() -> (PocketIc, Principal, Principal) {
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
    push_readings(&pic, canister, 19_000_000);
    (pic, canister, admin)
}

/// Keeps both chains' readings young at `head`, the way a live watcher would.
pub fn push_readings(pic: &PocketIc, canister: Principal, head: u64) {
    for chain in [BASE, ARBITRUM] {
        push_chain_data(
            pic,
            canister,
            watcher(),
            chain,
            &ChainData {
                block: head,
                base_fee_wei_per_gas: Nat::from(1_000_000_000_u64),
                priority_fee_wei_per_gas: Nat::from(100_000_000_u64),
            },
        )
        .expect("the watcher may push");
    }
}

/// The swap of `quote` as a claim would leave it: the deposit read is mocked in the claim
/// tests, so here the line is planted through the test door.
pub fn funded(
    pic: &PocketIc,
    canister: Principal,
    admin: Principal,
    quote: &types::Quote,
) -> Hash32 {
    let quote_hash = swap_id(quote);
    append(
        pic,
        canister,
        admin,
        &EventType::FundsReceived {
            quote_hash,
            quote_bytes: quote.canonical_bytes().unwrap(),
            chain_id: quote.src_chain.get(),
            token: quote.src_token.to_string(),
            amount: quote.amount_in.into(),
            tx_ref: "0xdeposit".into(),
        },
    )
    .expect("the swap is funded");
    quote_hash
}

pub fn events(pic: &PocketIc, canister: Principal) -> Vec<Event> {
    events_page(pic, canister, Principal::anonymous(), 0, 500)
}

/// Moves the clock, keeps the readings young, and gives the canister the rounds a tick,
/// a signature and an outcall need.
pub fn advance(pic: &PocketIc, canister: Principal, by: Duration, head: u64) {
    pic.advance_time(by);
    push_readings(pic, canister, head);
    for _ in 0..12 {
        pic.tick();
    }
}

pub fn methods(request: &CanisterHttpRequest) -> Vec<String> {
    let body: Value = serde_json::from_slice(&request.body).expect("the body is json");
    body.as_array()
        .expect("a batch is an array")
        .iter()
        .map(|call| call["method"].as_str().expect("a method").to_string())
        .collect()
}

pub fn reply(pic: &PocketIc, request: &CanisterHttpRequest, results: Vec<Value>) {
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
    for _ in 0..12 {
        pic.tick();
    }
}

fn created(events: &[Event]) -> Vec<TxPurpose> {
    events
        .iter()
        .filter_map(|event| match &event.payload {
            EventType::TxCreated { purpose, .. } => Some(*purpose),
            _ => None,
        })
        .collect()
}

/// The engine, on its timer, sends the rail's first leg for a claimed swap: a burn through
/// the source vault, decoding to `execute` of `depositForBurnWithHook` for the destination
/// vault, hooked with the swap's quote hash. It sends one leg and then waits on it: the
/// next tick allocates nothing more, because the attempt is open.
#[test]
fn the_engine_sends_the_burn_for_a_funded_swap_and_then_waits_on_it() {
    let (pic, canister, admin) = setup();
    let quote = quote(1);
    let quote_hash = funded(&pic, canister, admin, &quote);
    assert!(
        created(&events(&pic, canister)).is_empty(),
        "nothing before the tick"
    );

    advance(&pic, canister, TICK, 19_000_001);
    let log = events(&pic, canister);
    assert_eq!(
        created(&log),
        vec![TxPurpose::Burn(quote_hash)],
        "the burn, and only it"
    );
    let signed = log
        .iter()
        .filter(|event| matches!(event.payload, EventType::TxSigned { .. }))
        .count();
    assert_eq!(signed, 1, "signed through the one send path");
    let EventType::TxCreated {
        to, data, chain_id, ..
    } = log
        .iter()
        .find_map(|event| match &event.payload {
            payload @ EventType::TxCreated { .. } => Some(payload.clone()),
            _ => None,
        })
        .unwrap()
    else {
        unreachable!()
    };
    assert_eq!(chain_id, BASE);
    assert_eq!(to, VAULT_BASE);
    let execute = decode_vault_execute(&data).expect("an execute");
    let calls = execute.calls;
    assert_eq!(execute.swap_ref.into_bytes(), quote_hash);
    let burn = decode_cctp_deposit_for_burn_with_hook(&calls[0].data).expect("a hooked burn");
    assert_eq!(burn.destination_domain, 3);
    assert_eq!(burn.hook_data, quote_hash.to_vec());
    assert_eq!(
        burn.mint_recipient,
        VAULT_ARBITRUM
            .parse::<types::EvmAddress>()
            .unwrap()
            .to_word()
    );
    let swap = get_swap(&pic, canister, Principal::anonymous(), quote_hash).unwrap();
    assert_eq!(swap.status, SwapStatus::Executing);
    assert_eq!(swap.last_leg, Some(Leg::Burn));
    assert_eq!(swap.open_attempt, Some(1));

    // the burn goes out on the outbox's window, and the next tick sends nothing more
    advance(&pic, canister, Duration::from_secs(2), 19_000_001);
    let pending = pic.get_canister_http();
    assert_eq!(pending.len(), 1);
    assert_eq!(methods(&pending[0]), vec!["eth_sendRawTransaction"]);
    reply(&pic, &pending[0], vec![json!("0xabc")]);
    advance(&pic, canister, TICK, 19_000_002);
    for request in pic.get_canister_http() {
        // the receipt read, answered with nothing mined yet
        let results = methods(&request)
            .iter()
            .map(|method| match method.as_str() {
                "eth_blockNumber" => json!("0x121eac2"),
                _ => json!(null),
            })
            .collect();
        reply(&pic, &request, results);
    }
    assert_eq!(
        created(&events(&pic, canister)),
        vec![TxPurpose::Burn(quote_hash)],
        "one leg at a time"
    );
    assert!(verify_replay(&pic, canister, Principal::anonymous()));
}

/// A halted canister's engine sends nothing and records nothing: the tick runs and returns.
#[test]
fn a_halted_canister_drives_nothing() {
    let (pic, canister, admin) = setup();
    let quote = quote(2);
    funded(&pic, canister, admin, &quote);
    let before = events(&pic, canister).len();
    set_halted(&pic, canister, admin, true).unwrap();
    advance(&pic, canister, TICK, 19_000_001);
    advance(&pic, canister, TICK, 19_000_002);
    assert_eq!(events(&pic, canister).len(), before, "nothing while halted");
    assert!(pic.get_canister_http().is_empty());

    set_halted(&pic, canister, admin, false).unwrap();
    advance(&pic, canister, TICK, 19_000_003);
    assert_eq!(
        created(&events(&pic, canister)),
        vec![TxPurpose::Burn(swap_id(&quote))],
        "and the leg goes out once the halt lifts"
    );
}

/// A refund the canister cannot pay stops the swap for a human with the reason, rather
/// than being retried every tick: the quote names no refund address, so `send_refund`
/// freezes it with that. (A claim now refuses such a quote at the door; this swap is
/// written straight into the fold, which is the shape an older line could still hold.)
#[test]
fn a_refund_on_a_quote_with_no_refund_address_is_frozen_with_the_reason() {
    let (pic, canister, admin) = setup();
    let quote = types::Quote {
        refund_address: None,
        ..quote(3)
    };
    let quote_hash = funded(&pic, canister, admin, &quote);
    append(
        &pic,
        canister,
        admin,
        &EventType::RefundStarted {
            quote_hash,
            reason: "the user asked".into(),
        },
    )
    .unwrap();
    advance(&pic, canister, TICK, 19_000_001);
    let swap = get_swap(&pic, canister, Principal::anonymous(), quote_hash).unwrap();
    assert_eq!(swap.status, SwapStatus::Frozen);
    assert_eq!(
        events(&pic, canister).last().unwrap().payload,
        EventType::Frozen {
            quote_hash,
            reason: "the quote names no refund address".into(),
        }
    );
    assert!(pic.get_canister_http().is_empty(), "nothing was sent");
    assert!(verify_replay(&pic, canister, Principal::anonymous()));
}

/// Sanctions are checked where the money leaves, not only where the swap starts: an
/// address listed while the swap was executing (minutes on CCTP) is not paid, the swap
/// stops for a human with the party named, and nothing goes out.
#[test]
fn an_address_listed_after_the_claim_is_not_paid_out() {
    use crate::client::settlement::set_sanctioned;
    let (pic, canister, admin) = setup();
    let quote = quote(4);
    let quote_hash = funded(&pic, canister, admin, &quote);
    // the stable is on the destination side, so the next tick pays the user out
    append(
        &pic,
        canister,
        admin,
        &EventType::PaidInStable {
            quote_hash,
            chain_id: ARBITRUM,
            amount: Nat::from(24_995_000_u32),
        },
    )
    .expect("the swap is paid in stable");
    set_sanctioned(
        &pic,
        canister,
        watcher(),
        &[&USER.to_ascii_lowercase()],
        &[],
    )
    .expect("the watcher lists the address");

    advance(&pic, canister, TICK, 19_000_001);
    let swap = get_swap(&pic, canister, Principal::anonymous(), quote_hash).unwrap();
    assert_eq!(swap.status, SwapStatus::Frozen);
    assert_eq!(
        events(&pic, canister).last().unwrap().payload,
        EventType::Frozen {
            quote_hash,
            reason: "the quote's dst_address is sanctioned".into(),
        }
    );
    assert!(pic.get_canister_http().is_empty(), "nothing was sent");
    assert!(verify_replay(&pic, canister, Principal::anonymous()));
}

/// The same where the funds go back: a refund address listed after the claim is not paid
/// either, and the swap stops for a human rather than sending to it.
#[test]
fn a_refund_address_listed_after_the_claim_is_not_paid_out() {
    use crate::client::settlement::set_sanctioned;
    let (pic, canister, admin) = setup();
    let quote = quote(5);
    let quote_hash = funded(&pic, canister, admin, &quote);
    append(
        &pic,
        canister,
        admin,
        &EventType::RefundStarted {
            quote_hash,
            reason: "the user asked".into(),
        },
    )
    .expect("a funded swap can start a refund");
    set_sanctioned(
        &pic,
        canister,
        watcher(),
        &[&USER.to_ascii_lowercase()],
        &[],
    )
    .expect("the watcher lists the address");

    advance(&pic, canister, TICK, 19_000_001);
    let swap = get_swap(&pic, canister, Principal::anonymous(), quote_hash).unwrap();
    assert_eq!(swap.status, SwapStatus::Frozen);
    assert_eq!(
        events(&pic, canister).last().unwrap().payload,
        EventType::Frozen {
            quote_hash,
            reason: "the quote's refund_address is sanctioned".into(),
        }
    );
    assert!(pic.get_canister_http().is_empty(), "nothing was sent");
}

/// A burn that would deliver below the least the user was quoted is refused at the source,
/// never sent to freeze the swap at the destination (review 5, M4). A Standard swap the
/// quoter priced for no fee, with a slack of 1,000 units, on a chain whose messenger now
/// charges a minimum of one basis point (2,500 units of 25 USDC): the engine's first tick
/// starts the refund with the reason, before anything is signed, and the next sends the
/// refund from the source vault. No burn is ever created and the swap is never frozen.
#[test]
fn a_standard_swap_priced_for_no_fee_is_refunded_at_the_source_when_the_minimum_exceeds_its_slack()
{
    use crate::client::settlement::set_config;
    let (pic, canister, admin) = setup();
    set_config(
        &pic,
        canister,
        admin,
        &Config {
            cctp_min_fees: BTreeMap::from([(BASE, 1_000)]),
            ..config()
        },
    )
    .expect("the operator lists Base's minimum fee");
    let quote = types::Quote {
        rail: Rail::CctpV2Standard,
        expected_out: TokenAmount::from(AMOUNT),
        min_out: TokenAmount::from(AMOUNT - 1_000),
        ..quote(6)
    };
    let quote_hash = funded(&pic, canister, admin, &quote);

    advance(&pic, canister, TICK, 19_000_001);
    let log = events(&pic, canister);
    assert_eq!(created(&log), vec![], "no burn was created");
    assert_eq!(
        log.last().unwrap().payload,
        EventType::RefundStarted {
            quote_hash,
            reason: "a burn charged the 2500 it offers would pay the user out 24997500, \
                     below the 24999000 they were quoted"
                .into(),
        }
    );
    let swap = get_swap(&pic, canister, Principal::anonymous(), quote_hash).unwrap();
    assert_eq!(swap.status, SwapStatus::Refunding);

    advance(&pic, canister, TICK, 19_000_002);
    let log = events(&pic, canister);
    assert_eq!(
        created(&log),
        vec![TxPurpose::Refund(quote_hash)],
        "the refund from the source vault, and never a burn"
    );
    let EventType::TxCreated {
        to, data, chain_id, ..
    } = log
        .iter()
        .find_map(|event| match &event.payload {
            payload @ EventType::TxCreated { .. } => Some(payload.clone()),
            _ => None,
        })
        .unwrap()
    else {
        unreachable!()
    };
    assert_eq!(chain_id, BASE);
    assert_eq!(to, VAULT_BASE);
    let refund = types::abi::decode_vault_refund(&data).expect("a vault refund");
    assert_eq!(refund.amount, TokenAmount::from(AMOUNT));
    assert!(
        log.iter()
            .all(|event| !matches!(event.payload, EventType::Frozen { .. })),
        "never frozen"
    );
    assert!(verify_replay(&pic, canister, Principal::anonymous()));
}
