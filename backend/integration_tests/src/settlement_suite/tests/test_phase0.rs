//! Phase 0, end to end and fully mocked: a Base to Arbitrum USDC swap over CCTP walks the
//! whole event trail, and a refund walks its own.
//!
//! Every outcall the canister makes is answered by one responder that reads the JSON-RPC
//! methods out of the request body: the head block, the vault's deposit log, the
//! broadcasts (recorded, so the test can decode what was sent) and the receipts for what
//! was broadcast. Nothing else is stubbed: the claim, the engine, both rails' legs, the
//! outbox, the signatures and the fold all run as deployed.

use crate::client::settlement::{append, derive_evm_address};
use crate::client::settlement::{
    audit_replay_step, events_page, evm_address, get_swap, push_attestation, verify_chain,
    verify_replay,
};
use crate::settlement_suite::init::watcher;
use crate::settlement_suite::init::{install, quoter};
use crate::settlement_suite::tests::test_engine::{
    config, push_readings, quote, swap_id, MESSAGE_TRANSMITTER, TOKEN_MESSENGER, USDC_ARBITRUM,
    USDC_BASE, USER, VAULT_ARBITRUM, VAULT_BASE,
};
use alloy_primitives::keccak256;
use alloy_rlp::Header;
use candid::{encode_one, Principal};
use pocket_ic::common::rest::{CanisterHttpReply, CanisterHttpResponse, MockCanisterHttpResponse};
use pocket_ic::{PocketIc, PocketIcBuilder};
use serde_json::{json, Value};
use settlement_api::types::config::Config;
use settlement_api::types::events::{Event, EventType, Hash32, TxPurpose};
use settlement_api::types::init::InitArg;
use settlement_api::types::quote::Quote;
use settlement_api::types::swap::SwapStatus;
use std::collections::BTreeMap;
use std::time::Duration;
use types::abi::{
    decode_cctp_deposit_for_burn, decode_cctp_receive_message, decode_vault_execute,
    decode_vault_payout, decode_vault_refund, deposited_topic,
};
use types::EvmAddress;

/// How far the loop goes before it gives up on a swap that is not converging.
const MAX_ITERATIONS: usize = 120;

/// How much clock one iteration moves: enough for the outbox's window and the receipt
/// pass, with the engine's tick set to the same so every iteration is a full turn.
const STEP: Duration = Duration::from_secs(10);

/// A transaction as this canister signs it: the fields of the EIP-1559 envelope the test
/// decodes the broadcast bytes back into.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SentTx {
    chain_id: u64,
    nonce: u64,
    to: EvmAddress,
    value: u128,
    data: Vec<u8>,
    hash: Hash32,
}

/// The next RLP string item of `buf`: its header, then its payload.
fn item(buf: &mut &[u8]) -> Vec<u8> {
    let header = Header::decode(buf).expect("an rlp item");
    assert!(!header.list, "a string item, not a list");
    let (payload, rest) = buf.split_at(header.payload_length);
    *buf = rest;
    payload.to_vec()
}

/// An RLP integer: big-endian, without leading zeros, and no bytes at all for zero.
fn integer(bytes: &[u8]) -> u128 {
    bytes
        .iter()
        .fold(0_u128, |acc, byte| (acc << 8) | u128::from(*byte))
}

/// The raw bytes of a type-2 transaction, decoded field by field: the type byte, then the
/// RLP list of chain id, nonce, both fees, gas, to, value, data, the access list and the
/// signature.
fn decode_eip1559(raw: &[u8]) -> SentTx {
    assert_eq!(raw[0], 0x02, "an EIP-1559 transaction");
    let mut buf = &raw[1..];
    let header = Header::decode(&mut buf).expect("the envelope is an rlp list");
    assert!(header.list);
    let chain_id = integer(&item(&mut buf)) as u64;
    let nonce = integer(&item(&mut buf)) as u64;
    let _max_priority_fee = item(&mut buf);
    let _max_fee = item(&mut buf);
    let _gas_limit = item(&mut buf);
    let to = item(&mut buf);
    let value = integer(&item(&mut buf));
    let data = item(&mut buf);
    SentTx {
        chain_id,
        nonce,
        to: EvmAddress::new(to.as_slice().try_into().expect("a 20-byte address")),
        value,
        data,
        hash: keccak256(raw).0,
    }
}

/// Everything the mocked chains know: a head that grows with every answer, the deposits
/// the vaults hold, and every transaction that was broadcast with the block it was mined
/// in.
struct Chains {
    head: u64,
    deposits: Vec<Value>,
    sent: Vec<SentTx>,
    mined: BTreeMap<Hash32, u64>,
}

impl Chains {
    fn new(head: u64) -> Self {
        Self {
            head,
            deposits: Vec::new(),
            sent: Vec::new(),
            mined: BTreeMap::new(),
        }
    }

    /// The answer to one call: the head, the vault's logs, a broadcast recorded and mined
    /// in the next block, or a receipt for a hash this responder has seen.
    fn answer(&mut self, method: &str, params: &Value) -> Value {
        match method {
            "eth_blockNumber" => json!(format!("0x{:x}", self.head)),
            "eth_getLogs" => {
                let filter = &params[0];
                let wanted = filter["topics"][1].as_str().expect("a quote hash topic");
                let logs: Vec<Value> = self
                    .deposits
                    .iter()
                    .filter(|log| log["topics"][1] == wanted && log["address"] == filter["address"])
                    .cloned()
                    .collect();
                json!(logs)
            }
            "eth_sendRawTransaction" => {
                let raw =
                    hex::decode(params[0].as_str().unwrap().trim_start_matches("0x")).unwrap();
                let tx = decode_eip1559(&raw);
                let hash = tx.hash;
                self.mined.insert(hash, self.head + 1);
                self.sent.push(tx);
                json!(format!("0x{}", hex::encode(hash)))
            }
            "eth_getTransactionReceipt" => {
                let hash: Hash32 =
                    hex::decode(params[0].as_str().unwrap().trim_start_matches("0x"))
                        .unwrap()
                        .try_into()
                        .unwrap();
                match self.mined.get(&hash) {
                    Some(block) => json!({
                        "transactionHash": format!("0x{}", hex::encode(hash)),
                        "blockHash": format!("0x{}", hex::encode([0x42; 32])),
                        "blockNumber": format!("0x{block:x}"),
                        "status": "0x1",
                    }),
                    None => Value::Null,
                }
            }
            other => panic!("the canister asked for {other}, which the fixture does not answer"),
        }
    }

    /// Answers every pending outcall from the fixture, each by the methods in its body,
    /// and moves the head on.
    fn respond_all(&mut self, pic: &PocketIc) -> usize {
        let pending = pic.get_canister_http();
        let answered = pending.len();
        for request in pending {
            let body: Value = serde_json::from_slice(&request.body).expect("the body is json");
            let results: Vec<Value> = body
                .as_array()
                .expect("a batch")
                .iter()
                .enumerate()
                .map(|(id, call)| {
                    let result = self.answer(call["method"].as_str().unwrap(), &call["params"]);
                    json!({"jsonrpc": "2.0", "id": id, "result": result})
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
        self.head += 1;
        answered
    }
}

/// The vault's `Deposited` log for `quote_hash` on the source chain.
fn deposit_log(quote_hash: Hash32, block: u64, amount: u128) -> Value {
    let word = |address: &str| {
        format!(
            "0x{}",
            hex::encode(address.parse::<EvmAddress>().unwrap().to_word())
        )
    };
    json!({
        "address": VAULT_BASE.to_ascii_lowercase(),
        "topics": [
            format!("0x{}", hex::encode(deposited_topic())),
            format!("0x{}", hex::encode(quote_hash)),
            word(USDC_BASE),
            word(USER),
        ],
        "data": format!("0x{amount:064x}"),
        "blockNumber": format!("0x{block:x}"),
        "blockHash": format!("0x{}", hex::encode([0x41; 32])),
        "transactionHash": format!("0x{}", hex::encode([0x77; 32])),
        "logIndex": "0x0",
        "removed": false,
    })
}

/// The install: every rail knob set, the two mock chains, the engine on a ten second tick
/// so one iteration of the loop is one whole turn of the machine.
fn setup() -> (PocketIc, Principal, Principal) {
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
            rail_status_max_age_s: STEP.as_secs(),
            ..config()
        },
        quoter: quoter(),
        watcher: watcher(),
    };
    install(&pic, canister, admin, &arg).expect("the arg installs");
    derive_evm_address(&pic, canister, admin).expect("the test key derives an address");
    push_readings(&pic, canister, 19_000_000);
    (pic, canister, admin)
}

fn events(pic: &PocketIc, canister: Principal) -> Vec<Event> {
    events_page(pic, canister, Principal::anonymous(), 0, 500)
}

/// One line of the trail, as the assertions read it: the variant and, for the lines of an
/// attempt, its number or purpose.
fn shape(payload: &EventType) -> String {
    match payload {
        EventType::FundsReceived { .. } => "FundsReceived".into(),
        EventType::TxCreated { purpose, .. } => format!("TxCreated({})", purpose_name(*purpose)),
        EventType::TxSigned { attempt, .. } => format!("TxSigned({attempt})"),
        EventType::TxConfirmed { attempt, .. } => format!("TxConfirmed({attempt})"),
        EventType::TxFailed { attempt, .. } => format!("TxFailed({attempt})"),
        EventType::PaidInStable { .. } => "PaidInStable".into(),
        EventType::SwapDone { .. } => "SwapDone".into(),
        EventType::RefundStarted { .. } => "RefundStarted".into(),
        EventType::Refunded { .. } => "Refunded".into(),
        EventType::Frozen { reason, .. } => format!("Frozen({reason})"),
        other => format!("{other:?}"),
    }
}

fn purpose_name(purpose: TxPurpose) -> &'static str {
    match purpose {
        TxPurpose::Burn(_) => "Burn",
        TxPurpose::Mint(_) => "Mint",
        TxPurpose::Payout(_) => "Payout",
        TxPurpose::Refund(_) => "Refund",
        TxPurpose::GaslessPull(_) => "GaslessPull",
        TxPurpose::Cancel(_) => "Cancel",
        TxPurpose::Reclaim(_) => "Reclaim",
    }
}

/// The trail of one swap: every line after the install's, in order.
fn trail(pic: &PocketIc, canister: Principal, installed: usize) -> Vec<String> {
    events(pic, canister)[installed..]
        .iter()
        .map(|event| shape(&event.payload))
        .collect()
}

/// Claims the quote against the fixture's deposit: submits the claim, answers its read,
/// and returns the swap id.
fn claim(pic: &PocketIc, canister: Principal, chains: &mut Chains, quote: &types::Quote) -> Hash32 {
    let quote_hash = swap_id(quote);
    chains.deposits.push(deposit_log(
        quote_hash,
        chains.head,
        quote.amount_in.try_into_u128().unwrap(),
    ));
    let call = pic
        .submit_call(
            canister,
            watcher(),
            "claim_swap",
            encode_one(Quote::from(quote.clone())).unwrap(),
        )
        .unwrap();
    pic.tick();
    pic.tick();
    assert_eq!(chains.respond_all(pic), 1, "the claim reads once");
    for _ in 0..4 {
        pic.tick();
    }
    let claimed: Result<Hash32, settlement_api::types::entry::ClaimError> =
        candid::decode_one(&pic.await_call(call).expect("the claim returns")).unwrap();
    assert_eq!(claimed, Ok(quote_hash));
    quote_hash
}

/// Turns the machine until the swap reaches `until`, answering every outcall and handing
/// in the attestation once the burn has confirmed, or fails readably with the trail so far.
fn drive_until(
    pic: &PocketIc,
    canister: Principal,
    chains: &mut Chains,
    quote_hash: Hash32,
    installed: usize,
    until: SwapStatus,
    attestation: Option<(&[u8], &[u8])>,
) {
    let mut pushed = false;
    for iteration in 0..MAX_ITERATIONS {
        pic.advance_time(STEP);
        push_readings(pic, canister, chains.head);
        for _ in 0..12 {
            pic.tick();
        }
        chains.respond_all(pic);
        for _ in 0..12 {
            pic.tick();
        }
        chains.respond_all(pic);
        let burn_confirmed = events(pic, canister).iter().any(|event| {
            matches!(&event.payload, EventType::TxConfirmed { quote_hash: qh, attempt: 1, .. } if *qh == quote_hash)
        });
        if let (Some((message, attestation)), true, false) = (attestation, burn_confirmed, pushed) {
            push_attestation(pic, canister, watcher(), quote_hash, message, attestation)
                .expect("the watcher hands in the attestation");
            pushed = true;
        }
        let swap =
            get_swap(pic, canister, Principal::anonymous(), quote_hash).expect("the swap exists");
        if swap.status == until {
            return;
        }
        assert!(
            !matches!(
                swap.status,
                SwapStatus::Frozen | SwapStatus::Done | SwapStatus::Refunded
            ),
            "iteration {iteration}: the swap closed as {:?} instead of {until:?}; trail: {:#?}",
            swap.status,
            trail(pic, canister, installed)
        );
    }
    panic!(
        "the swap did not reach {until:?} in {MAX_ITERATIONS} iterations of {STEP:?}; trail: {:#?}; \
         sent: {:#?}",
        trail(pic, canister, installed),
        chains.sent
    );
}

/// The deep audit, run to its verdict.
fn deep_audit_is_clean(pic: &PocketIc, canister: Principal, admin: Principal) -> bool {
    for _ in 0..100 {
        let progress = audit_replay_step(pic, canister, admin, 1_000).expect("a controller audits");
        if progress.finished {
            return progress.matches && !progress.halted;
        }
        assert!(!progress.halted, "the audit halted: {progress:?}");
    }
    panic!("the deep audit did not finish");
}

/// The Phase 0 trail, exactly: the deposit becomes the swap, the burn is created, signed
/// and confirmed, the attestation is handed in, the mint is created, signed and confirmed
/// and lands the stable, the payout is created, signed and confirmed, and the swap is done.
/// The three broadcasts decode to the vault's `execute` of `depositForBurn`, the
/// transmitter's `receiveMessage` and the vault's `payout`, and the log holds up under
/// every audit.
#[test]
fn base_to_arbitrum_usdc_walks_the_whole_event_trail() {
    let (pic, canister, admin) = setup();
    let mine: EvmAddress = evm_address(&pic, canister, admin)
        .expect("derived")
        .parse()
        .unwrap();
    let installed = events(&pic, canister).len();
    let mut chains = Chains::new(19_000_000);
    let quote = quote(1);
    let quote_hash = claim(&pic, canister, &mut chains, &quote);

    let message = vec![0xaa; 376];
    let attestation = vec![0xbb; 65];
    drive_until(
        &pic,
        canister,
        &mut chains,
        quote_hash,
        installed,
        SwapStatus::Done,
        Some((&message, &attestation)),
    );

    assert_eq!(
        trail(&pic, canister, installed),
        vec![
            "FundsReceived",
            "TxCreated(Burn)",
            "TxSigned(1)",
            "TxConfirmed(1)",
            "TxCreated(Mint)",
            "TxSigned(2)",
            "TxConfirmed(2)",
            "PaidInStable",
            "TxCreated(Payout)",
            "TxSigned(3)",
            "TxConfirmed(3)",
            "SwapDone",
        ]
    );
    let paid = events(&pic, canister)
        .iter()
        .find_map(|event| match &event.payload {
            EventType::PaidInStable {
                chain_id, amount, ..
            } => Some((*chain_id, amount.clone())),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        paid,
        (42161, candid::Nat::from(24_995_000_u32)),
        "the stable landed on Arbitrum, the burn less the fast fee's ceiling"
    );

    // the three broadcasts, decoded
    assert_eq!(
        chains.sent.len(),
        3,
        "three transactions: {:#?}",
        chains.sent
    );
    let burn = &chains.sent[0];
    assert_eq!((burn.chain_id, burn.nonce), (8453, 0));
    assert_eq!(burn.to, VAULT_BASE.parse().unwrap());
    let (swap_ref, calls, _) = decode_vault_execute(&burn.data).expect("an execute");
    assert_eq!(swap_ref.into_bytes(), quote_hash);
    assert_eq!(calls[0].target, TOKEN_MESSENGER.parse().unwrap());
    let inner = decode_cctp_deposit_for_burn(&calls[0].data).expect("a depositForBurn");
    assert_eq!(inner.amount, quote.amount_in);
    assert_eq!(inner.destination_domain, 3);
    assert_eq!(
        inner.mint_recipient,
        VAULT_ARBITRUM.parse::<EvmAddress>().unwrap().to_word()
    );
    assert_eq!(inner.destination_caller, mine.to_word());
    assert_eq!(inner.min_finality_threshold, 1_000);

    let mint = &chains.sent[1];
    assert_eq!((mint.chain_id, mint.nonce), (42161, 0));
    assert_eq!(mint.to, MESSAGE_TRANSMITTER.parse().unwrap());
    assert_eq!(
        decode_cctp_receive_message(&mint.data),
        Some((message.clone(), attestation.clone())),
        "the mint carries what the watcher handed in"
    );

    let payout = &chains.sent[2];
    assert_eq!((payout.chain_id, payout.nonce), (42161, 1));
    assert_eq!(payout.to, VAULT_ARBITRUM.parse().unwrap());
    assert_eq!(
        decode_vault_payout(&payout.data),
        Some((
            types::QuoteHash::new(quote_hash),
            USDC_ARBITRUM.parse().unwrap(),
            USER.parse().unwrap(),
            types::TokenAmount::from(24_995_000_u32),
        ))
    );
    assert!(chains.sent.iter().all(|tx| tx.value == 0));

    // and the log holds up
    assert!(verify_chain(&pic, canister, Principal::anonymous()));
    assert!(verify_replay(&pic, canister, Principal::anonymous()));
    assert!(deep_audit_is_clean(&pic, canister, admin));
    assert!(
        pic.get_canister_http().is_empty(),
        "nothing is left in flight"
    );
}

/// The refund trail: a refund started on a funded swap sends the vault's `refund` of the
/// whole deposit to the quote's refund address, and its confirmation closes the swap as
/// refunded.
#[test]
fn a_refund_walks_to_refunded() {
    let (pic, canister, admin) = setup();
    let installed = events(&pic, canister).len();
    let mut chains = Chains::new(19_000_000);
    let quote = quote(2);
    let quote_hash = claim(&pic, canister, &mut chains, &quote);
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

    drive_until(
        &pic,
        canister,
        &mut chains,
        quote_hash,
        installed,
        SwapStatus::Refunded,
        None,
    );
    assert_eq!(
        trail(&pic, canister, installed),
        vec![
            "FundsReceived",
            "RefundStarted",
            "TxCreated(Refund)",
            "TxSigned(1)",
            "TxConfirmed(1)",
            "Refunded",
        ]
    );
    assert_eq!(chains.sent.len(), 1);
    let refund = &chains.sent[0];
    assert_eq!((refund.chain_id, refund.nonce), (8453, 0));
    assert_eq!(refund.to, VAULT_BASE.parse().unwrap());
    assert_eq!(
        decode_vault_refund(&refund.data),
        Some((
            types::QuoteHash::new(quote_hash),
            USDC_BASE.parse().unwrap(),
            USER.parse().unwrap(),
            quote.amount_in,
        ))
    );
    let refunded = events(&pic, canister).last().unwrap().payload.clone();
    assert_eq!(
        refunded,
        EventType::Refunded {
            quote_hash,
            chain_id: 8453,
            token: USDC_BASE.to_string(),
            amount: quote.amount_in.into(),
            to: USER.to_string(),
        }
    );
    assert!(verify_chain(&pic, canister, Principal::anonymous()));
    assert!(verify_replay(&pic, canister, Principal::anonymous()));
    assert!(deep_audit_is_clean(&pic, canister, admin));
}
