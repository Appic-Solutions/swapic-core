//! Design C end to end on one canister: a gasless order relayed as `Swapic.runFor`, its
//! CCTP arrival finished as `CctpReceiver.completeCctp`, both read back from `signed_txs`
//! as the watcher reads them, decoded to the calldata they must carry, and recovered to the
//! canister's own address. The order is the quoter's real fork order (a gasless USDC to
//! CCTP Swap from Arbitrum), its burn re-targeted to Base and its deadlines sealed from
//! this instance's clock, as the quoter seals them; the message is Circle's real one, re-made
//! as that burn. The only outcalls are the outbox's sends and receipt reads, answered
//! here: the canister simulates nothing of its own.

use crate::client::pocket::{query, update};
use crate::client::settlement::{derive_evm_address, push_chain_data};
use crate::settlement_suite::init::{init_arg, install, quoter, watcher};
use alloy_primitives::U256;
use alloy_sol_types::SolCall;
use candid::{encode_args, Nat, Principal};
use pocket_ic::common::rest::{CanisterHttpReply, CanisterHttpResponse, MockCanisterHttpResponse};
use pocket_ic::{PocketIc, PocketIcBuilder};
use serde_json::{json, Value};
use settlement_api::types::chain_data::ChainData;
use settlement_api::types::config::{ActionAmounts, ActionGas, Config, EvmChainConfig};
use settlement_api::types::flow::Recipe;
use settlement_api::types::init::InitArg;
use settlement_api::types::tx::{ChainRef, SignedTx};
use std::collections::BTreeMap;
use types::cctp::BurnMessage;
use types::swapic::{self, Envelope, GaslessAuth, Order, DOOR_AUTH_3009};
use types::{ChainId, EvmAddress};

const BASE: u64 = 8453;
const ARBITRUM: u64 = 42161;
/// The fork deploy's contracts, one address on every chain.
const SWAPIC: &str = "0xF2E9d13Ba690c71cfF7788DE7273Ca7658d6D7C7";
const RUNNER: &str = "0xd9a6d4123BcF737A36e5f66D474891f15E8d2d3C";
const MESSENGER: &str = "0x28b5a0e9C621a5BadaA536219b3a228C8168cf5d";
const TRANSMITTER: &str = "0x81D40F21F12A8F0E3252Bccb954D722d4c464B64";
const BASE_USDC: &str = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913";
const ARB_USDC: &str = "0xaf88d065e77c8cC2239327C5EDb3A432268e5831";
/// The limits the quoter charged the real order's fee on.
const SOURCE_GAS: u64 = 393_566;
const ARRIVAL_GAS: u64 = 1_185_547;

fn address(text: &str) -> EvmAddress {
    text.parse().unwrap()
}

fn sample_order() -> Value {
    serde_json::from_str(include_str!(
        "../../../../impl/samples/quoter_order_arbitrum_cctp_gasless.json"
    ))
    .unwrap()
}

fn hex_field(json: &Value, field: &str) -> Vec<u8> {
    hex::decode(json[field].as_str().unwrap().trim_start_matches("0x")).unwrap()
}

/// The fork deploy's CctpReceiver: what the real order burns to.
fn receiver() -> EvmAddress {
    let order = swapic::decode_order(&hex_field(&sample_order(), "orderAbi")).unwrap();
    let burn = swapic::CctpBurn::of_step(&order.steps[1], address(MESSENGER))
        .unwrap()
        .unwrap();
    swapic::evm_of_word(burn.mint_recipient).unwrap()
}

fn chain(usdc: &str) -> EvmChainConfig {
    let amounts = |value: u128| ActionAmounts {
        relay_cctp: Nat::from(value),
        relay_eco: Nat::from(value),
        relay_local: Nat::from(value),
        complete_swap: Nat::from(value),
        complete_ck: Nat::from(value),
        relay_deliver: Nat::from(value),
        release: Nat::from(value),
    };
    EvmChainConfig {
        swapic: SWAPIC.to_string(),
        runner: RUNNER.to_string(),
        cctp_receiver: receiver().to_string(),
        usdc: usdc.to_string(),
        token_messenger: MESSENGER.to_string(),
        message_transmitter: TRANSMITTER.to_string(),
        gas_limit: ActionGas {
            relay_cctp: 600_000,
            relay_eco: 600_000,
            relay_local: 900_000,
            complete_swap: 1_300_000,
            complete_ck: 600_000,
            relay_deliver: 300_000,
            release: 300_000,
        },
        // a tenth of an ether: the gas rule holds at the fees pushed below
        gas_budget_wei: amounts(100_000_000_000_000_000),
        // the relay on Arbitrum and the completion on Base, inside the 166,275 charged
        min_network_fee: ActionAmounts {
            relay_cctp: Nat::from(100_000_u32),
            complete_swap: Nat::from(60_000_u32),
            ..amounts(0)
        },
    }
}

/// Base and Arbitrum as design C runs on them, with a provider each for the outbox.
fn config() -> Config {
    Config {
        evm_chains: BTreeMap::from([(BASE, chain(BASE_USDC)), (ARBITRUM, chain(ARB_USDC))]),
        cctp_domains: BTreeMap::from([(BASE, 6), (ARBITRUM, 3)]),
        rpc_urls: BTreeMap::from([
            (BASE, "https://base-mainnet.example/v2/key".to_string()),
            (ARBITRUM, "https://arb-mainnet.example/v2/key".to_string()),
        ]),
        ..Config::default()
    }
}

/// (pic, canister, admin) on a network holding pocket-ic's test threshold keys.
fn setup() -> (PocketIc, Principal, Principal) {
    let pic = PocketIcBuilder::new()
        .with_ii_subnet()
        .with_application_subnet()
        .build();
    let admin = Principal::from_slice(&[1; 29]);
    let subnet = pic.topology().get_app_subnets()[0];
    let canister = pic.create_canister_on_subnet(Some(admin), None, subnet);
    pic.add_cycles(canister, 100_000_000_000_000);
    let arg = InitArg {
        config: config(),
        ..init_arg()
    };
    install(&pic, canister, admin, &arg).expect("the design C config installs");
    (pic, canister, admin)
}

fn push(pic: &PocketIc, canister: Principal, chain_id: u64) {
    push_chain_data(
        pic,
        canister,
        watcher(),
        chain_id,
        &ChainData {
            block: 19_000_000,
            base_fee_wei_per_gas: Nat::from(10_000_000_u64),
            priority_fee_wei_per_gas: Nat::from(1_000_000_u64),
        },
    )
    .expect("the watcher may push");
}

fn now_s(pic: &PocketIc) -> u64 {
    pic.get_time().as_nanos_since_unix_epoch() / 1_000_000_000
}

/// The real order, its fee paying this canister, its burn sent to Base, its deadlines
/// sealed from `now`: the order deadline ten minutes on, the arrival window half an hour.
fn order_for(canister: EvmAddress, now: u64) -> (Order, Envelope) {
    let json = sample_order();
    let mut order = swapic::decode_order(&hex_field(&json, "orderAbi")).unwrap();
    order.deadline = U256::from(now + 600);
    let fee: U256 = json["networkFee"].as_str().unwrap().parse().unwrap();
    order.steps[0].data = swapic::transferCall {
        to: swapic::addr(canister),
        amount: fee,
    }
    .abi_encode()
    .into();
    let mut burn = swapic::depositForBurnWithHookCall::abi_decode(&order.steps[1].data).unwrap();
    let mut envelope = Envelope::decode(&burn.hookData).unwrap();
    envelope.deadline = now + 1_800;
    burn.destinationDomain = 6;
    burn.mintRecipient = receiver().to_word().into();
    burn.destinationCaller = receiver().to_word().into();
    burn.hookData = envelope.encode().into();
    order.steps[1].data = burn.abi_encode().into();
    (order, envelope)
}

/// Circle's real attested message (Base to Arbitrum), re-made as the order's burn
/// arriving on Base: burned on Arbitrum by its Runner, minted to and called by Base's
/// CctpReceiver, carrying the order's envelope.
fn message_for(envelope: &Envelope) -> (Vec<u8>, Vec<u8>) {
    let json: Value = serde_json::from_str(include_str!(
        "../../../../impl/samples/cctp_base_to_arbitrum.json"
    ))
    .unwrap();
    let iris = |field: &str| {
        hex::decode(
            json["iris"][field]
                .as_str()
                .unwrap()
                .trim_start_matches("0x"),
        )
        .unwrap()
    };
    let mut message = BurnMessage::parse(&iris("message")).unwrap();
    message.source_domain = 3;
    message.destination_domain = 6;
    message.body.burn_token = address(ARB_USDC).to_word();
    message.body.message_sender = address(RUNNER).to_word();
    message.body.mint_recipient = receiver().to_word();
    message.destination_caller = receiver().to_word();
    message.body.hook_data = envelope.encode();
    (message.encode(), iris("attestation"))
}

/// The items of one RLP list: each item's own encoding, header included.
fn rlp_items(list: &[u8]) -> Vec<&[u8]> {
    let length = |bytes: &[u8]| bytes.iter().fold(0_usize, |n, b| n * 256 + *b as usize);
    let (header, payload) = match list[0] {
        0xc0..=0xf7 => (1, (list[0] - 0xc0) as usize),
        b => {
            let n = (b - 0xf7) as usize;
            (1 + n, length(&list[1..1 + n]))
        }
    };
    let mut items = Vec::new();
    let mut at = header;
    while at < header + payload {
        let size = match list[at] {
            0x00..=0x7f => 1,
            b @ 0x80..=0xb7 => 1 + (b - 0x80) as usize,
            b @ 0xb8..=0xbf => {
                let n = (b - 0xb7) as usize;
                1 + n + length(&list[at + 1..at + 1 + n])
            }
            b @ 0xc0..=0xf7 => 1 + (b - 0xc0) as usize,
            b => {
                let n = (b - 0xf7) as usize;
                1 + n + length(&list[at + 1..at + 1 + n])
            }
        };
        items.push(&list[at..at + size]);
        at += size;
    }
    items
}

/// An RLP string's payload.
fn rlp_bytes(item: &[u8]) -> &[u8] {
    match item[0] {
        0x00..=0x7f => item,
        b @ 0x80..=0xb7 => &item[1..1 + (b - 0x80) as usize],
        b => &item[1 + (b - 0xb7) as usize..],
    }
}

fn integer(item: &[u8]) -> u64 {
    rlp_bytes(item)
        .iter()
        .fold(0_u64, |n, b| n * 256 + u64::from(*b))
}

/// A signed EIP-1559 transaction read back: its chain, `to`, gas limit and data, and the
/// address its signature recovers to.
struct Read {
    chain_id: u64,
    to: Vec<u8>,
    gas_limit: u64,
    data: Vec<u8>,
    signer: String,
}

fn read(raw: &[u8]) -> Read {
    assert_eq!(raw[0], 0x02, "an EIP-1559 envelope");
    let items = rlp_items(&raw[1..]);
    assert_eq!(items.len(), 12);
    // the nine unsigned fields, under a list header of their own, are what was signed
    let unsigned: Vec<u8> = items[..9].concat();
    let mut payload = vec![0x02];
    if unsigned.len() < 56 {
        payload.push(0xc0 + unsigned.len() as u8);
    } else {
        let length = unsigned.len().to_be_bytes();
        let length: Vec<u8> = length.iter().copied().skip_while(|b| *b == 0).collect();
        payload.push(0xf7 + length.len() as u8);
        payload.extend_from_slice(&length);
    }
    payload.extend_from_slice(&unsigned);
    let hash: [u8; 32] = alloy_primitives::keccak256(&payload).into();
    let mut signature = [0_u8; 64];
    let (r, s) = (rlp_bytes(items[10]), rlp_bytes(items[11]));
    signature[32 - r.len()..32].copy_from_slice(r);
    signature[64 - s.len()..].copy_from_slice(s);
    let key = libsecp256k1::recover(
        &libsecp256k1::Message::parse(&hash),
        &libsecp256k1::Signature::parse_standard(&signature).unwrap(),
        &libsecp256k1::RecoveryId::parse(integer(items[9]) as u8).unwrap(),
    )
    .unwrap();
    let digest = alloy_primitives::keccak256(&key.serialize()[1..]);
    Read {
        chain_id: integer(items[0]),
        to: rlp_bytes(items[5]).to_vec(),
        gas_limit: integer(items[4]),
        data: rlp_bytes(items[7]).to_vec(),
        signer: format!("0x{}", hex::encode(&digest[12..])),
    }
}

/// Answers every outcall the outbox makes (a send with its transaction's hash, anything
/// else with a word that decides nothing), and answers which methods it saw.
fn answer_broadcasts(pic: &PocketIc) -> Vec<String> {
    let mut methods = Vec::new();
    for _ in 0..10 {
        pic.advance_time(std::time::Duration::from_secs(3));
        pic.tick();
        let pending = pic.get_canister_http();
        if pending.is_empty() {
            continue;
        }
        for request in pending {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            let calls = body.as_array().unwrap();
            let results: Vec<Value> = calls
                .iter()
                .enumerate()
                .map(|(id, call)| {
                    methods.push(call["method"].as_str().unwrap().to_string());
                    let raw = hex::decode(
                        call["params"][0]
                            .as_str()
                            .unwrap_or("0x")
                            .trim_start_matches("0x"),
                    )
                    .unwrap_or_default();
                    let hash = alloy_primitives::keccak256(&raw);
                    json!({"jsonrpc": "2.0", "id": id, "result": format!("0x{}", hex::encode(hash))})
                })
                .collect();
            pic.mock_canister_http_response(MockCanisterHttpResponse {
                subnet_id: request.subnet_id,
                request_id: request.request_id,
                response: CanisterHttpResponse::CanisterHttpReply(CanisterHttpReply {
                    status: 200,
                    headers: vec![],
                    body: Value::Array(results).to_string().into_bytes(),
                }),
                additional_responses: vec![],
            });
        }
    }
    methods
}

#[test]
fn a_gasless_order_and_its_arrival_are_signed_for_the_watcher() {
    let (pic, canister, admin) = setup();
    let mine = derive_evm_address(&pic, canister, admin).expect("the test key derives");
    let canister_address = address(&mine);
    let now = now_s(&pic);

    // the relay, as the submit API asks for it: the order and the 3009 authorization,
    // each `abi.encode` of one tuple, and the limit the quoter charged for
    let (order, envelope) = order_for(canister_address, now);
    let order_id = swapic::order_id(&order, ChainId::new(ARBITRUM), address(SWAPIC));
    let auth = GaslessAuth {
        door: DOOR_AUTH_3009,
        validAfter: U256::ZERO,
        validBefore: order.deadline,
        nonce: order_id.as_nonce(),
        deadline: U256::ZERO,
        signature: vec![0x1b; 65].into(),
        acceptance: Default::default(),
    };
    let relay_args = encode_args((
        ARBITRUM,
        swapic::encode_order(&order),
        swapic::encode_auth(&auth),
        SOURCE_GAS,
    ))
    .unwrap();
    let refused: Result<u64, String> = update(
        &pic,
        canister,
        quoter(),
        "relay_gasless",
        relay_args.clone(),
    );
    assert!(refused.is_err(), "the watcher's door: {refused:?}");
    push(&pic, canister, ARBITRUM);
    let relay_id: u64 = update::<Result<u64, String>>(
        &pic,
        canister,
        watcher(),
        "relay_gasless",
        relay_args.clone(),
    )
    .expect("the real order is relayed");
    let again: Result<u64, String> = update(&pic, canister, watcher(), "relay_gasless", relay_args);
    assert_eq!(again, Ok(relay_id), "one relay per order id");

    // the arrival on Base, before its deadline: completed with the stored recipe
    let (message, attestation) = message_for(&envelope);
    assert_eq!(message.len(), 696);
    let stored = swapic::decode_recipe(&hex_field(&sample_order(), "recipe")).unwrap();
    let recipe = Recipe {
        target: swapic::evm(stored.target).to_string(),
        data: stored.data.to_vec(),
        scaler: Some(swapic::evm(stored.scaler).to_string()),
    };
    let finish_args = encode_args((
        BASE,
        message.clone(),
        attestation.clone(),
        Some(recipe),
        vec![0x5c_u8; 32],
        ARRIVAL_GAS,
    ))
    .unwrap();
    push(&pic, canister, BASE);
    let finish_id: u64 = update::<Result<u64, String>>(
        &pic,
        canister,
        watcher(),
        "finish_cctp",
        finish_args.clone(),
    )
    .expect("the arrival is completed");
    let again: Result<u64, String> = update(&pic, canister, watcher(), "finish_cctp", finish_args);
    assert_eq!(again, Ok(finish_id), "one finish per message and action");

    // both are in `signed_txs`, as the watcher pages them
    let signed: Vec<SignedTx> = query(
        &pic,
        canister,
        watcher(),
        "signed_txs",
        encode_args((0_u64, 10_u64)).unwrap(),
    );
    let ids: Vec<u64> = signed.iter().map(|tx| tx.id).collect();
    assert_eq!(ids, vec![relay_id, finish_id]);
    assert_eq!(signed[0].chain, ChainRef::Evm(ARBITRUM));
    assert_eq!(signed[1].chain, ChainRef::Evm(BASE));

    // the relay: runFor(order, auth) to Arbitrum's Swapic at exactly the charged limit,
    // signed by the canister
    let relay = read(&signed[0].raw);
    assert_eq!(relay.chain_id, ARBITRUM);
    assert_eq!(relay.to, address(SWAPIC).as_bytes().to_vec());
    assert_eq!(relay.gas_limit, SOURCE_GAS);
    assert_eq!(relay.signer, mine.to_lowercase());
    let call = swapic::runForCall::abi_decode(&relay.data).unwrap();
    assert_eq!((call.order, call.auth), (order, auth));
    assert_eq!(
        alloy_primitives::keccak256(&signed[0].raw).to_vec(),
        signed[0].hash
    );

    // the finish: completeCctp(message, attestation, recipe) to Base's CctpReceiver at
    // exactly the charged limit, signed by the canister
    let finish = read(&signed[1].raw);
    assert_eq!(finish.chain_id, BASE);
    assert_eq!(finish.to, receiver().as_bytes().to_vec());
    assert_eq!(finish.gas_limit, ARRIVAL_GAS);
    assert_eq!(finish.signer, mine.to_lowercase());
    let call = swapic::completeCctpCall::abi_decode(&finish.data).unwrap();
    assert_eq!(call.message.to_vec(), message);
    assert_eq!(call.attestation.to_vec(), attestation);
    assert_eq!(call.recipe, stored);

    // the canister's outcalls are its outbox's, the backup to the watcher's broadcast:
    // the sends and the receipt reads, and no simulation of its own
    let methods = answer_broadcasts(&pic);
    assert!(
        methods
            .iter()
            .any(|method| method == "eth_sendRawTransaction"),
        "the outbox broadcasts: {methods:?}"
    );
    assert!(
        !methods
            .iter()
            .any(|method| method == "eth_call" || method == "eth_estimateGas"),
        "{methods:?}"
    );
}
