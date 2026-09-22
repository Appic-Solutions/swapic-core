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
    config, push_readings, quote, swap_id, AMOUNT, MESSAGE_TRANSMITTER, TOKEN_MESSENGER,
    USDC_ARBITRUM, USDC_BASE, USER, VAULT_ARBITRUM, VAULT_BASE,
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
    decode_vault_payout, decode_vault_refund, deposited_topic, mint_and_withdraw_topic,
};
use types::cctp::{BurnBody, BurnMessage, BURN_BODY_VERSION, MESSAGE_VERSION};
use types::{BlockNumber, EvmAddress, TokenAmount};

/// The fee Circle takes from the fixture's fast burn of 25 USDC: one basis point, inside
/// the two the burn allowed, so what the mint delivers is the burn less this.
const FEE_EXECUTED: u32 = 2_500;

/// The fee Circle takes from a burn of `amount`: one basis point of it.
fn fee_executed(amount: TokenAmount) -> TokenAmount {
    amount.checked_div_floor(10_000_u16).unwrap()
}

/// The attestation bytes the fixture hands in beside a message: the chain is mocked, so
/// nothing verifies them.
const ATTESTATION: [u8; 65] = [0xbb; 65];

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
                        "logs": self.logs_of(hash),
                    }),
                    None => Value::Null,
                }
            }
            other => panic!("the canister asked for {other}, which the fixture does not answer"),
        }
    }

    /// What a transaction logged: a mint (a `receiveMessage` to the transmitter) logs the
    /// token messenger's `MintAndWithdraw` to the destination vault of the burn less the
    /// fee the attested message carries, and everything else logs nothing the canister
    /// reads.
    fn logs_of(&self, hash: Hash32) -> Vec<Value> {
        let Some(tx) = self.sent.iter().find(|tx| tx.hash == hash) else {
            return vec![];
        };
        if tx.to != MESSAGE_TRANSMITTER.parse::<EvmAddress>().unwrap() {
            return vec![];
        }
        let (message, _) = decode_cctp_receive_message(&tx.data).expect("a receiveMessage");
        let message = BurnMessage::parse(&message).expect("a burn message");
        let delivered = message
            .body
            .amount
            .checked_sub(message.body.fee_executed)
            .unwrap();
        let word = |address: &str| {
            format!(
                "0x{}",
                hex::encode(address.parse::<EvmAddress>().unwrap().to_word())
            )
        };
        vec![json!({
            "address": TOKEN_MESSENGER.to_ascii_lowercase(),
            "topics": [
                format!("0x{}", hex::encode(mint_and_withdraw_topic())),
                word(VAULT_ARBITRUM),
                word(USDC_ARBITRUM),
            ],
            "data": format!(
                "0x{}{}",
                hex::encode(delivered.to_be_bytes()),
                hex::encode(message.body.fee_executed.to_be_bytes())
            ),
            "logIndex": "0x2",
            "removed": false,
        })]
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

/// The message Circle attests for the fixture's fast burn of `quote`: every field as the
/// burn emitted it, and the nonce, the fee and the expiration as the service fills them.
fn attested_message(quote: &types::Quote, mine: EvmAddress) -> BurnMessage {
    let word = |address: &str| address.parse::<EvmAddress>().unwrap().to_word();
    BurnMessage {
        version: MESSAGE_VERSION,
        source_domain: 6,
        destination_domain: 3,
        nonce: [0x9a; 32],
        sender: word(TOKEN_MESSENGER),
        recipient: word(TOKEN_MESSENGER),
        destination_caller: mine.to_word(),
        min_finality_threshold: 1_000,
        finality_threshold_executed: 1_000,
        body: BurnBody {
            version: BURN_BODY_VERSION,
            burn_token: word(USDC_BASE),
            mint_recipient: word(VAULT_ARBITRUM),
            amount: quote.amount_in,
            message_sender: word(VAULT_BASE),
            max_fee: quote
                .amount_in
                .checked_mul(2_u8)
                .and_then(|scaled| scaled.checked_div_ceil(10_000_u16))
                .unwrap(),
            fee_executed: fee_executed(quote.amount_in),
            expiration_block: BlockNumber::new(19_000_500),
            hook_data: vec![],
        },
    }
}

/// The transaction the swap's burn confirmed as, once it has.
fn burn_hash(pic: &PocketIc, canister: Principal, quote_hash: Hash32) -> Option<Hash32> {
    events(pic, canister)
        .iter()
        .find_map(|event| match &event.payload {
            EventType::TxConfirmed {
                quote_hash: qh,
                attempt: 1,
                tx_hash,
                ..
            } if *qh == quote_hash => Some(*tx_hash),
            _ => None,
        })
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

/// One whole turn of the machine: the clock moves, the readings stay young, the tick and
/// the outbox pass run, and every outcall they made is answered.
fn turn(pic: &PocketIc, canister: Principal, chains: &mut Chains) {
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
}

/// Turns the machine until the swap reaches `until`, answering every outcall and handing
/// in the attested `message` for the swap's burn once the burn has confirmed, or fails
/// readably with the trail so far.
fn drive_until(
    pic: &PocketIc,
    canister: Principal,
    chains: &mut Chains,
    quote_hash: Hash32,
    installed: usize,
    until: SwapStatus,
    message: Option<&BurnMessage>,
) {
    let mut pushed = false;
    for iteration in 0..MAX_ITERATIONS {
        turn(pic, canister, chains);
        if let (Some(message), Some(burn), false) =
            (message, burn_hash(pic, canister, quote_hash), pushed)
        {
            push_attestation(
                pic,
                canister,
                watcher(),
                quote_hash,
                burn,
                &message.encode(),
                &ATTESTATION,
            )
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

/// Turns the machine until the swap's burn has confirmed, and answers the transaction it
/// confirmed as.
fn drive_until_burn_confirmed(
    pic: &PocketIc,
    canister: Principal,
    chains: &mut Chains,
    quote_hash: Hash32,
    installed: usize,
) -> Hash32 {
    for _ in 0..MAX_ITERATIONS {
        turn(pic, canister, chains);
        if let Some(burn) = burn_hash(pic, canister, quote_hash) {
            return burn;
        }
    }
    panic!(
        "the burn did not confirm in {MAX_ITERATIONS} iterations of {STEP:?}; trail: {:#?}",
        trail(pic, canister, installed)
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
/// and confirmed, the attestation is handed in, the mint is created, signed and confirmed,
/// what it delivered is read off its receipt and lands the stable, the payout is created,
/// signed and confirmed, and the swap is done. The three broadcasts decode to the vault's
/// `execute` of `depositForBurn`, the transmitter's `receiveMessage` and the vault's
/// `payout`, and the log holds up under every audit.
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

    let message = attested_message(&quote, mine);
    drive_until(
        &pic,
        canister,
        &mut chains,
        quote_hash,
        installed,
        SwapStatus::Done,
        Some(&message),
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
        (42161, candid::Nat::from(AMOUNT - FEE_EXECUTED)),
        "the stable landed on Arbitrum: what the mint's receipt says was delivered, the \
         burn less the fee Circle took, and never what the burn promised"
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
        Some((message.encode(), ATTESTATION.to_vec())),
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
            types::TokenAmount::from(AMOUNT - FEE_EXECUTED),
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

/// An attestation is bound to its swap: two swaps burn, and the message of one pushed
/// under the other's hash is refused by the field that differs, as is a push naming a burn
/// that is not the swap's own; each swap's own message is taken in, the same push twice
/// changes nothing, and each swap is paid what its own mint delivered.
#[test]
fn a_message_of_another_swap_is_refused_and_each_swap_is_paid_its_own_mint() {
    use settlement_api::types::entry::{
        MessageField, MessageMismatch, PushAttestationError, RailError,
    };
    let (pic, canister, admin) = setup();
    let mine: EvmAddress = evm_address(&pic, canister, admin)
        .expect("derived")
        .parse()
        .unwrap();
    let installed = events(&pic, canister).len();
    let mut chains = Chains::new(19_000_000);
    let big = quote(3);
    let small = types::Quote {
        amount_in: TokenAmount::from(10_000_000_u32),
        expected_out: TokenAmount::from(9_996_000_u32),
        min_out: TokenAmount::from(9_960_000_u32),
        ..quote(4)
    };
    let big_hash = claim(&pic, canister, &mut chains, &big);
    let small_hash = claim(&pic, canister, &mut chains, &small);
    let big_burn = drive_until_burn_confirmed(&pic, canister, &mut chains, big_hash, installed);
    let small_burn = drive_until_burn_confirmed(&pic, canister, &mut chains, small_hash, installed);
    assert_ne!(big_burn, small_burn);

    let big_message = attested_message(&big, mine).encode();
    let small_message = attested_message(&small, mine).encode();
    // the big swap's message under the small swap's hash, naming the small swap's burn:
    // refused by the amount, which is the field that differs
    assert_eq!(
        push_attestation(
            &pic,
            canister,
            watcher(),
            small_hash,
            small_burn,
            &big_message,
            &ATTESTATION
        ),
        Err(PushAttestationError::Rail(RailError::Message(
            MessageMismatch::Amount {
                field: MessageField::Amount,
                expected: candid::Nat::from(10_000_000_u32),
                found: candid::Nat::from(AMOUNT),
            }
        )))
    );
    // and naming the big swap's burn: refused as not the swap's own burn
    assert_eq!(
        push_attestation(
            &pic,
            canister,
            watcher(),
            small_hash,
            big_burn,
            &big_message,
            &ATTESTATION
        ),
        Err(PushAttestationError::NotTheSwapsBurn {
            pushed: big_burn,
            confirmed: small_burn,
        })
    );
    for (quote_hash, burn, message) in [
        (big_hash, big_burn, &big_message),
        (small_hash, small_burn, &small_message),
    ] {
        for _ in 0..2 {
            assert_eq!(
                push_attestation(
                    &pic,
                    canister,
                    watcher(),
                    quote_hash,
                    burn,
                    message,
                    &ATTESTATION
                ),
                Ok(()),
                "the swap's own message is taken in, the same push twice without complaint"
            );
        }
    }
    for quote_hash in [big_hash, small_hash] {
        drive_until(
            &pic,
            canister,
            &mut chains,
            quote_hash,
            installed,
            SwapStatus::Done,
            None,
        );
    }
    let paid: Vec<(Hash32, candid::Nat)> = events(&pic, canister)
        .iter()
        .filter_map(|event| match &event.payload {
            EventType::PaidInStable {
                quote_hash, amount, ..
            } => Some((*quote_hash, amount.clone())),
            _ => None,
        })
        .collect();
    let mut expected = vec![
        (big_hash, candid::Nat::from(AMOUNT - FEE_EXECUTED)),
        (small_hash, candid::Nat::from(10_000_000_u32 - 1_000)),
    ];
    let mut paid = paid;
    paid.sort();
    expected.sort();
    assert_eq!(
        paid, expected,
        "each swap is paid what its own mint delivered"
    );
    assert!(verify_replay(&pic, canister, Principal::anonymous()));
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
