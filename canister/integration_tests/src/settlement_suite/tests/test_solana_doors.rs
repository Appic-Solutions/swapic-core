//! The Solana doors that read the chain, whole: a stand-in for the SOL RPC canister answers fixed bytes,
//! the source chain's receipt is a mocked outcall, the signatures are pocket-ic's real test keys. What
//! the rules beside each door hold is tested there; this is that each door reaches them and signs.

use super::test_solana::{
    config, derive, evm_chain, gasless_tx, lifetime, query, setup, signed_txs, update,
};
use crate::client::settlement::{derive_evm_address, get_config_full, set_config};
use base64::Engine;
use candid::{decode_one, encode_args, encode_one, CandidType, Nat, Principal};
use pocket_ic::common::rest::{CanisterHttpReply, CanisterHttpResponse, MockCanisterHttpResponse};
use pocket_ic::PocketIc;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use settlement_api::types::solana::{
    FinishSolanaArgs, ProposeSolanaUpgradeArgs, RelaySolanaForwardArgs, ReleaseMisroutedArgs,
    SolanaBudget, SolanaFinishAction, SolanaProgramConfigArgs, SolanaReclaimItem, SolanaRoute,
    SolanaRouteAccount, SolanaUpgradeView, SweepSolanaFeesArgs,
};
use settlement_api::types::tx::SignedTx;
use std::time::Duration;
use swapic_solana_wire::base58;
use swapic_solana_wire::consts::{shard_of, TMM, TOKEN_PROGRAM, UPGRADEABLE_LOADER, USDC_MINT};
use swapic_solana_wire::envelope::{ForwarderHook, SolanaEnvelope, SOLANA_ACTION_SWAP};
use swapic_solana_wire::message::V1Message;
use swapic_solana_wire::pda::{ata, SwapicAddresses};
use types::cctp::BurnMessage;
use types::swapic;

const CHAIN: u64 = 8453;
const WALLET: [u8; 32] = [9; 32];
const REF: [u8; 32] = [5; 32];

// ---------- what the SOL RPC canister answers: its interface, as much of it as the canister reads

#[derive(CandidType)]
enum Answer<T> {
    Ok(T),
    #[allow(dead_code)]
    Err(String),
}

#[derive(CandidType)]
enum Multi<T> {
    Consistent(Answer<T>),
    #[allow(dead_code)]
    Inconsistent(String),
}

#[derive(CandidType, serde::Serialize)]
enum AccountData {
    #[serde(rename = "binary")]
    Binary(String, String),
}

#[derive(CandidType)]
struct AccountInfo {
    lamports: u64,
    data: AccountData,
    owner: String,
    space: u64,
}

#[derive(CandidType, serde::Serialize)]
enum EncodedTransaction {
    #[serde(rename = "binary")]
    Binary(String, String),
}

#[derive(CandidType)]
#[allow(dead_code)]
enum Status {
    Ok,
    Err(String),
}

#[derive(CandidType)]
struct Meta {
    status: Status,
}

#[derive(CandidType)]
struct WithMeta {
    meta: Option<Meta>,
    transaction: EncodedTransaction,
}

#[derive(CandidType)]
#[allow(non_snake_case)]
struct Confirmed {
    transaction: WithMeta,
    blockTime: Option<i64>,
}

/// What the stand-in answers, whatever it is asked.
struct Chain {
    account: Option<AccountInfo>,
    balance: u64,
    landed: Option<Status>,
}

impl Default for Chain {
    fn default() -> Self {
        Self {
            account: None,
            balance: 0,
            landed: None,
        }
    }
}

fn account_of(owner: &[u8; 32], data: &[u8]) -> AccountInfo {
    AccountInfo {
        lamports: 1,
        data: AccountData::Binary(
            base64::engine::general_purpose::STANDARD.encode(data),
            "base64".to_string(),
        ),
        owner: base58::encode(owner),
        space: 1 << 20,
    }
}

/// A module that answers each method with fixed bytes, and its cost at nothing.
fn stand_in(pic: &PocketIc, admin: Principal, chain: Chain) -> Principal {
    let confirmed = chain.landed.map(|status| Confirmed {
        transaction: WithMeta {
            meta: Some(Meta { status }),
            transaction: EncodedTransaction::Binary(String::new(), "base64".to_string()),
        },
        blockTime: Some(1),
    });
    let answers = [
        (
            "getAccountInfo",
            encode_one(Multi::Consistent(Answer::Ok(chain.account))).unwrap(),
        ),
        (
            "getBalance",
            encode_one(Multi::Consistent(Answer::Ok(chain.balance))).unwrap(),
        ),
        (
            "getTransaction",
            encode_one(Multi::Consistent(Answer::Ok(confirmed))).unwrap(),
        ),
    ];
    let cost = encode_one(Answer::Ok(Nat::from(0u32))).unwrap();
    let (mut blob, mut funcs) = (cost.clone(), String::new());
    let piece = |at: usize, len: usize| {
        format!("(call $append (i32.const {at}) (i32.const {len})) (call $reply)")
    };
    for (name, answer) in &answers {
        funcs += &format!(
            "(func (export \"canister_query {name}CyclesCost\") {})\n",
            piece(0, cost.len())
        );
        funcs += &format!(
            "(func (export \"canister_update {name}\") {})\n",
            piece(blob.len(), answer.len())
        );
        blob.extend_from_slice(answer);
    }
    let data: String = blob.iter().map(|b| format!("\\{b:02x}")).collect();
    let module = format!(
        "(module
           (import \"ic0\" \"msg_reply_data_append\" (func $append (param i32 i32)))
           (import \"ic0\" \"msg_reply\" (func $reply))
           (memory 1)
           (data (i32.const 0) \"{data}\")
           {funcs})"
    );
    let subnet = pic.topology().get_app_subnets()[0];
    let id = pic.create_canister_on_subnet(Some(admin), None, subnet);
    pic.add_cycles(id, 10_000_000_000_000);
    pic.install_canister(id, wat::parse_str(module).unwrap(), vec![], Some(admin));
    id
}

// ---------- the door, the message, the receipt

struct Door {
    pic: PocketIc,
    canister: Principal,
    admin: Principal,
    watcher: Principal,
    a: SwapicAddresses,
    evm: types::EvmAddress,
}

fn door_on(chain: impl FnOnce(&SwapicAddresses) -> Chain) -> Door {
    let (pic, canister, admin) = setup();
    let keys = derive(&pic, canister, admin);
    let a = SwapicAddresses::derive(
        [1; 32],
        base58::decode_pubkey(&keys.payer).unwrap(),
        base58::decode_pubkey(&keys.admin).unwrap(),
    );
    let evm = derive_evm_address(&pic, canister, admin).expect("the test key derives");
    let rpc = stand_in(&pic, admin, chain(&a));
    let mut full = get_config_full(&pic, canister, admin).unwrap();
    full.cctp_domains.insert(CHAIN, 6);
    full.evm_chains.insert(CHAIN, evm_chain());
    full.rpc_urls
        .insert(CHAIN, "https://base-mainnet.example/v2/key".to_string());
    let mut solana = config();
    solana.sol_rpc = rpc;
    full.solana = Some(solana);
    set_config(&pic, canister, admin, &full).unwrap();
    Door {
        pic,
        canister,
        admin,
        watcher: crate::settlement_suite::init::watcher(),
        a,
        evm: evm_address(&evm),
    }
}

fn evm_address(text: &str) -> types::EvmAddress {
    let bytes: [u8; 20] = hex::decode(text.trim_start_matches("0x"))
        .unwrap()
        .try_into()
        .unwrap();
    types::EvmAddress::new(bytes)
}

fn iris(field: &str) -> Vec<u8> {
    let json: Value = serde_json::from_str(include_str!(
        "../../../../impl/samples/cctp_base_to_arbitrum.json"
    ))
    .unwrap();
    hex::decode(
        json["iris"][field]
            .as_str()
            .unwrap()
            .trim_start_matches("0x"),
    )
    .unwrap()
}

/// A burn for Solana from `evm_chain()`'s Runner, attested under `nonce`.
fn message_of(nonce: u8, caller: [u8; 32], mint_recipient: [u8; 32], hook: Vec<u8>) -> Vec<u8> {
    let chain = evm_chain();
    let mut m = BurnMessage::parse(&iris("message")).unwrap();
    m.source_domain = 6;
    m.destination_domain = 5;
    m.nonce = [nonce; 32];
    m.sender = evm_address(&chain.token_messenger).to_word();
    m.recipient = TMM;
    m.destination_caller = caller;
    m.body.burn_token = evm_address(&chain.usdc).to_word();
    m.body.mint_recipient = mint_recipient;
    m.body.message_sender = evm_address(&chain.runner).to_word();
    m.body.hook_data = hook;
    m.encode()
}

fn hook_of(d: &Door, recipient: [u8; 32], ahead_s: u64) -> Vec<u8> {
    SolanaEnvelope {
        ref_bytes: REF,
        action: SOLANA_ACTION_SWAP,
        deadline: (now_s(d) + ahead_s) as u32,
        recipient,
        mint_out: [0x77; 32],
        min_out: 1_000,
    }
    .encode()
    .to_vec()
}

fn now_s(d: &Door) -> u64 {
    d.pic.get_time().as_nanos_since_unix_epoch() / 1_000_000_000
}

/// A message into the program for `hook`, from the ref's own shard.
fn program_message(d: &Door, nonce: u8, hook: Vec<u8>) -> Vec<u8> {
    let shard = shard_of(&REF);
    message_of(nonce, d.a.auth[shard].0, d.a.vault[shard], hook)
}

/// The source transaction's receipt: `message` sent, and the Runner's fee step to the canister.
fn receipt_of(d: &Door, message: &[u8], fee: u64) -> Value {
    use alloy_sol_types::SolEvent;
    let chain = evm_chain();
    let mut emitted = BurnMessage::parse(message).unwrap();
    emitted.nonce = [0; 32];
    emitted.finality_threshold_executed = 0;
    emitted.body.fee_executed = types::TokenAmount::ZERO;
    emitted.body.expiration_block = types::BlockNumber::new(0);
    let word = |address: types::EvmAddress| format!("0x{}", hex::encode(address.to_word()));
    let mut logs = vec![];
    if fee > 0 {
        logs.push(json!({
            "address": chain.usdc,
            "topics": [
                format!("0x{}", hex::encode(swapic::topics::transfer())),
                word(evm_address(&chain.runner)),
                word(d.evm),
            ],
            "data": format!("0x{fee:064x}"),
            "logIndex": "0x1",
            "removed": false,
        }));
    }
    let sent = swapic::MessageSent {
        message: emitted.encode().into(),
    };
    logs.push(json!({
        "address": chain.message_transmitter,
        "topics": [format!("0x{}", hex::encode(swapic::topics::message_sent()))],
        "data": format!("0x{}", hex::encode(sent.encode_data())),
        "logIndex": "0x4",
        "removed": false,
    }));
    json!({
        "transactionHash": format!("0x{}", "5c".repeat(32)),
        "blockNumber": "0x1000",
        "status": "0x1",
        "logs": logs,
    })
}

/// A watcher's call, the source's receipt answered if it is asked for.
fn asked<T: DeserializeOwned + CandidType>(
    d: &Door,
    method: &str,
    args: Vec<u8>,
    receipt: &Value,
) -> T {
    let call = d
        .pic
        .submit_call(d.canister, d.watcher, method, args)
        .expect("the door takes the call");
    for _ in 0..12 {
        d.pic.tick();
        let read = d.pic.get_canister_http().into_iter().find(|request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            body[0]["method"] == "eth_getTransactionReceipt"
        });
        if let Some(request) = read {
            d.pic.mock_canister_http_response(MockCanisterHttpResponse {
                subnet_id: request.subnet_id,
                request_id: request.request_id,
                response: CanisterHttpResponse::CanisterHttpReply(CanisterHttpReply {
                    status: 200,
                    headers: vec![],
                    body: json!([{"jsonrpc": "2.0", "id": 0, "result": receipt}])
                        .to_string()
                        .into_bytes(),
                }),
                additional_responses: vec![],
            });
            break;
        }
    }
    decode_one(&d.pic.await_call(call).expect("the door answers")).unwrap()
}

fn finish_args(message: &[u8], action: SolanaFinishAction, route: Option<SolanaRoute>) -> Vec<u8> {
    encode_one(FinishSolanaArgs {
        message: message.to_vec(),
        attestation: vec![0; 65],
        src_tx_hash: vec![0x5c; 32],
        action,
        route,
        cu_limit: 400_000,
        create_account: true,
        token_2022: false,
        lifetime: lifetime(),
    })
    .unwrap()
}

fn finish(
    d: &Door,
    message: &[u8],
    action: SolanaFinishAction,
    route: Option<SolanaRoute>,
    receipt: &Value,
) -> Result<Option<u64>, String> {
    asked(d, "finish_solana", finish_args(message, action, route), receipt)
}

/// The transaction a line holds, as the wire crate reads it back.
fn signed(d: &Door, id: u64) -> V1Message {
    let tx: SignedTx = signed_txs(&d.pic, d.canister, d.admin)
        .into_iter()
        .find(|tx| tx.id == id)
        .expect("the line is there");
    V1Message::parse_wire(&tx.raw).unwrap().0
}

fn lines(d: &Door) -> usize {
    signed_txs(&d.pic, d.canister, d.admin).len()
}

fn set_program_paused(d: &Door) {
    let answer: Result<u64, String> = update(
        &d.pic,
        d.canister,
        d.admin,
        "set_solana_program_config",
        encode_one(SolanaProgramConfigArgs {
            paused: true,
            event_keypair_allowed: false,
            swap_domains: 1 << 6,
            ck_domains: 1,
            min_burn: 1_000_000,
            min_deadline_s: 600,
            max_deadline_s: 7_200,
            lifetime: lifetime(),
        })
        .unwrap(),
    );
    answer.unwrap();
}

/// A route the canister's rule 7 reads: Jupiter's `route_v2`, naming no account of Swapic's.
fn route() -> SolanaRoute {
    let mut data = hex::decode("bb64facc31c4af14").unwrap();
    data.extend_from_slice(&[0; 24]);
    SolanaRoute {
        data,
        accounts: (40u8..48)
            .map(|byte| SolanaRouteAccount {
                address: base58::encode(&[byte; 32]),
                writable: true,
            })
            .collect(),
    }
}

// ---------- the arrival doors

#[test]
fn a_release_is_signed_only_from_a_source_that_paid_and_one_fee_pays_one_arrival() {
    let d = door_on(|_| Chain::default());
    let first = program_message(&d, 1, hook_of(&d, WALLET, 3_600));

    // the same burn, with the canister paid nothing
    let unpaid: Result<Option<u64>, String> =
        finish(&d, &first, SolanaFinishAction::Release, None, &receipt_of(&d, &first, 0));
    assert!(unpaid.unwrap_err().contains("no fee step"));
    assert_eq!(lines(&d), 0);

    let paid = receipt_of(&d, &first, 1_000);
    let id = finish(&d, &first, SolanaFinishAction::Release, None, &paid)
        .unwrap()
        .expect("the release is signed");
    let tx = signed(&d, id);
    assert_eq!(tx.addresses[0], d.a.canister, "the payer key pays");
    assert!(tx.addresses.contains(&ata(&WALLET, &USDC_MINT, &TOKEN_PROGRAM)));
    assert!(tx.addresses.contains(&d.a.program));

    // an identical unpaid burn under another nonce, named on the paid transaction: the receipt does not
    // tell the two apart, the one fee does
    let twin = program_message(&d, 2, hook_of(&d, WALLET, 3_600));
    let refused = finish(&d, &twin, SolanaFinishAction::Release, None, &paid);
    assert!(refused.unwrap_err().contains("already finished"));

    // the paid arrival itself keeps its tries, up to its cap
    for _ in 0..2 {
        finish(&d, &first, SolanaFinishAction::Release, None, &paid)
            .unwrap()
            .expect("a retry of the same arrival");
    }
    let spent = finish(&d, &first, SolanaFinishAction::Release, None, &paid);
    assert!(spent.unwrap_err().contains("spent"));
}

#[test]
fn a_complete_signs_the_route_and_stops_at_a_pause_a_shard_mismatch_and_the_margin() {
    let d = door_on(|_| Chain::default());
    let message = program_message(&d, 1, hook_of(&d, WALLET, 3_600));
    let paid = receipt_of(&d, &message, 1_000);

    let no_route = finish(&d, &message, SolanaFinishAction::Complete, None, &paid);
    assert!(no_route.unwrap_err().contains("names a route"));
    let id = finish(&d, &message, SolanaFinishAction::Complete, Some(route()), &paid)
        .unwrap()
        .expect("the complete is signed");
    let tx = signed(&d, id);
    assert_eq!(tx.addresses[0], d.a.canister);
    assert!(tx.addresses.contains(&[40; 32]), "the route is in it");

    // past the margin
    let close = program_message(&d, 3, hook_of(&d, WALLET, 30));
    let late = finish(&d, &close, SolanaFinishAction::Complete, Some(route()), &paid);
    assert!(late.unwrap_err().contains("margin"));

    // across shards: only a release, which the program pays
    let shard = shard_of(&REF);
    let across = message_of(
        4,
        d.a.auth[shard].0,
        d.a.vault[(shard + 1) % 4],
        hook_of(&d, WALLET, 3_600),
    );
    let refused = finish(&d, &across, SolanaFinishAction::Complete, Some(route()), &paid);
    assert!(refused.unwrap_err().contains("different shards"));

    // a pause: only a release
    set_program_paused(&d);
    let paused = finish(&d, &message, SolanaFinishAction::Complete, Some(route()), &paid);
    assert!(paused.unwrap_err().contains("paused"));
}

#[test]
fn a_release_across_shards_is_signed_and_a_program_address_goes_to_the_hand_door() {
    let d = door_on(|_| Chain::default());
    let shard = shard_of(&REF);
    let across = message_of(
        4,
        d.a.auth[shard].0,
        d.a.vault[(shard + 1) % 4],
        hook_of(&d, WALLET, 3_600),
    );
    let paid = receipt_of(&d, &across, 1_000);
    let id = finish(&d, &across, SolanaFinishAction::Release, None, &paid)
        .unwrap()
        .expect("a release takes any two shards");
    assert!(signed(&d, id)
        .addresses
        .contains(&d.a.vault[(shard + 1) % 4]));

    // the program pays a recipient of its own nothing: finish refuses, the hand door pays the fee account
    let own = program_message(&d, 5, hook_of(&d, d.a.config.0, 3_600));
    let refused = finish(&d, &own, SolanaFinishAction::Release, None, &paid);
    assert!(refused.unwrap_err().contains("release_solana_misrouted"));

    let misrouted = |message: &[u8]| -> Vec<u8> {
        encode_one(ReleaseMisroutedArgs {
            message: message.to_vec(),
            attestation: vec![0; 65],
            order_ref: vec![1; 32],
            payer: "0x1111111111111111111111111111111111111111".to_string(),
            lifetime: lifetime(),
        })
        .unwrap()
    };
    let stranger: Result<Option<u64>, String> = update(
        &d.pic,
        d.canister,
        d.watcher,
        "release_solana_misrouted",
        misrouted(&own),
    );
    assert!(stranger.is_err());
    let id: Result<Option<u64>, String> = update(
        &d.pic,
        d.canister,
        d.admin,
        "release_solana_misrouted",
        misrouted(&own),
    );
    assert!(signed(&d, id.unwrap().unwrap())
        .addresses
        .contains(&d.a.fee_account));

    // an unreadable hook, the same
    let junk = program_message(&d, 6, vec![9; 10]);
    let id: Result<Option<u64>, String> = update(
        &d.pic,
        d.canister,
        d.admin,
        "release_solana_misrouted",
        misrouted(&junk),
    );
    assert!(signed(&d, id.unwrap().unwrap())
        .addresses
        .contains(&d.a.fee_account));

    // a hook that names a wallet: the hand door never takes it
    let wallet = program_message(&d, 7, hook_of(&d, WALLET, 3_600));
    let refused: Result<Option<u64>, String> = update(
        &d.pic,
        d.canister,
        d.admin,
        "release_solana_misrouted",
        misrouted(&wallet),
    );
    assert!(refused.unwrap_err().contains("names a recipient"));
}

#[test]
fn a_message_already_received_signs_nothing_ever_again() {
    let d = door_on(|_| Chain {
        account: Some(account_of(&[3; 32], &[])),
        ..Chain::default()
    });
    let message = program_message(&d, 1, hook_of(&d, WALLET, 3_600));
    let paid = receipt_of(&d, &message, 1_000);
    let first = finish(&d, &message, SolanaFinishAction::Release, None, &paid);
    assert_eq!(first, Ok(None));
    let again = finish(&d, &message, SolanaFinishAction::Complete, Some(route()), &paid);
    assert_eq!(again, Ok(None));
    assert_eq!(lines(&d), 0);
}

#[test]
fn a_forwarder_message_is_relayed_twice_at_most_and_one_fee_pays_one_relay() {
    let d = door_on(|_| Chain::default());
    let mut hook = [0u8; 65];
    let len = ForwarderHook::CreateFor(WALLET).write(&mut hook);
    let account = ata(&WALLET, &USDC_MINT, &TOKEN_PROGRAM);
    let message = message_of(1, [0; 32], account, hook[..len].to_vec());
    let paid = receipt_of(&d, &message, 1_000);
    let relay = |message: &[u8]| -> Result<Option<u64>, String> {
        asked(
            &d,
            "relay_solana_forward",
            encode_one(RelaySolanaForwardArgs {
                message: message.to_vec(),
                attestation: vec![0; 65],
                src_tx_hash: vec![0x5c; 32],
                lifetime: lifetime(),
            })
            .unwrap(),
            &paid,
        )
    };
    let id = relay(&message).unwrap().expect("the relay is signed");
    assert!(signed(&d, id).addresses.contains(&account));

    let twin = message_of(2, [0; 32], account, hook[..len].to_vec());
    assert!(relay(&twin).unwrap_err().contains("already finished"));

    relay(&message).unwrap().expect("its one retry");
    assert!(relay(&message).unwrap_err().contains("spent"));

    // the program's own messages have a caller, and are never relayed
    let own = program_message(&d, 3, hook_of(&d, WALLET, 3_600));
    assert!(relay(&own).unwrap_err().contains("destination caller"));
}

// ---------- what follows a co-signature

fn cosign(d: &Door, r: [u8; 32], user: &ed25519_dalek::SigningKey) -> Result<u64, String> {
    let tx = gasless_tx(&d.a, user, (now_s(d) + 1_800) as u32, r, 2_000_000);
    update(
        &d.pic,
        d.canister,
        d.watcher,
        "cosign_solana",
        encode_args((tx, 123u64)).unwrap(),
    )
}

fn report(d: &Door, signature: &[u8]) -> Result<bool, String> {
    update(
        &d.pic,
        d.canister,
        d.watcher,
        "report_solana_failure",
        encode_one(signature.to_vec()).unwrap(),
    )
}

fn budget(d: &Door) -> SolanaBudget {
    query(&d.pic, d.canister, "solana_budget", encode_args(()).unwrap())
}

fn hash_of(d: &Door, id: u64) -> Vec<u8> {
    signed_txs(&d.pic, d.canister, d.admin)
        .into_iter()
        .find(|tx| tx.id == id)
        .unwrap()
        .hash
}

#[test]
fn a_landed_failure_blocks_its_signer_and_is_charged_once() {
    let d = door_on(|_| Chain {
        landed: Some(Status::Err("custom program error".to_string())),
        ..Chain::default()
    });
    let user = ed25519_dalek::SigningKey::from_bytes(&[42; 32]);
    let id = cosign(&d, REF, &user).expect("co-signed");
    assert_eq!(budget(&d).cosigns_this_hour, 1);

    // not one the canister signed
    assert!(report(&d, &[1; 64]).unwrap_err().contains("did not co-sign"));

    let signature = hash_of(&d, id);
    assert_eq!(report(&d, &signature), Ok(true));
    assert_eq!(budget(&d).failures_this_hour, 1);
    // a retried report answers the same and charges nothing
    assert_eq!(report(&d, &signature), Ok(true));
    assert_eq!(budget(&d).failures_this_hour, 1);

    // its signer gets no more, and another does
    let blocked = cosign(&d, [6; 32], &user);
    assert!(blocked.unwrap_err().contains("landed and failed"));
    let other = ed25519_dalek::SigningKey::from_bytes(&[43; 32]);
    cosign(&d, [7; 32], &other).expect("another signer is served");
}

#[test]
fn a_co_signed_transaction_that_landed_well_blocks_nobody() {
    let d = door_on(|_| Chain {
        landed: Some(Status::Ok),
        ..Chain::default()
    });
    let user = ed25519_dalek::SigningKey::from_bytes(&[42; 32]);
    let id = cosign(&d, REF, &user).expect("co-signed");
    assert_eq!(report(&d, &hash_of(&d, id)), Ok(false));
    assert_eq!(budget(&d).failures_this_hour, 0);
    cosign(&d, [6; 32], &user).expect("still served");
}

#[test]
fn a_reclaim_waits_five_days_names_only_recorded_refs_and_tries_twice() {
    let d = door_on(|_| Chain::default());
    let user = ed25519_dalek::SigningKey::from_bytes(&[42; 32]);
    cosign(&d, REF, &user).expect("co-signed");
    let reclaim = |r: [u8; 32]| -> Result<u64, String> {
        update(
            &d.pic,
            d.canister,
            d.watcher,
            "reclaim_solana",
            encode_args((
                vec![SolanaReclaimItem {
                    r#ref: r.to_vec(),
                    attestation: vec![0; 65],
                    destination_message: vec![0; 300],
                }],
                lifetime(),
            ))
            .unwrap(),
        )
    };
    assert!(reclaim([8; 32]).unwrap_err().contains("not one the canister recorded"));
    assert!(reclaim(REF).unwrap_err().contains("five days"));

    d.pic.advance_time(Duration::from_secs(5 * 24 * 3_600 + 1));
    d.pic.tick();
    let id = reclaim(REF).expect("after five days");
    let tx = signed(&d, id);
    assert_eq!(tx.addresses[0], d.a.canister);
    assert!(tx.addresses.contains(&d.a.event(&REF)));
    reclaim(REF).expect("its one retry");
    assert!(reclaim(REF).unwrap_err().contains("tried twice"));
}

// ---------- the keeper and the controller's doors

#[test]
fn a_top_up_goes_to_the_float_at_most_once_an_hour() {
    let d = door_on(|_| Chain::default());
    let top_up = || -> Result<Option<u64>, String> {
        update(
            &d.pic,
            d.canister,
            d.watcher,
            "top_up_solana_rent",
            encode_one(lifetime()).unwrap(),
        )
    };
    let id = top_up().unwrap().expect("the float is below its floor");
    let tx = signed(&d, id);
    assert_eq!(tx.addresses[0], d.a.canister);
    assert!(tx.addresses.contains(&d.a.rent.0));
    assert_eq!(top_up(), Ok(None), "within the hour");
    d.pic.advance_time(Duration::from_secs(3_601));
    d.pic.tick();
    top_up().unwrap().expect("an hour later");
}

#[test]
fn a_full_float_is_not_topped_up() {
    let d = door_on(|_| Chain {
        balance: 1_000_000_000,
        ..Chain::default()
    });
    let answer: Result<Option<u64>, String> = update(
        &d.pic,
        d.canister,
        d.watcher,
        "top_up_solana_rent",
        encode_one(lifetime()).unwrap(),
    );
    assert_eq!(answer, Ok(None));
}

#[test]
fn only_a_controller_sweeps_and_it_is_signed_by_the_admin_key() {
    let d = door_on(|_| Chain::default());
    let args = encode_one(SweepSolanaFeesArgs {
        to: base58::encode(&[12; 32]),
        amount: 5_000_000,
        lifetime: lifetime(),
    })
    .unwrap();
    let refused: Result<u64, String> =
        update(&d.pic, d.canister, d.watcher, "sweep_solana_fees", args.clone());
    assert!(refused.is_err());
    let id: Result<u64, String> = update(&d.pic, d.canister, d.admin, "sweep_solana_fees", args);
    let tx = signed(&d, id.unwrap());
    assert_eq!(tx.addresses[0], d.a.canister, "the payer key pays");
    assert_eq!(tx.addresses[1], d.a.admin, "the admin key signs");
    assert!(tx.addresses.contains(&d.a.fee_account));
}

// ---------- the upgrade

fn buffered_build(d_admin: &[u8; 32], program: &[u8]) -> Vec<u8> {
    let mut data = vec![1, 0, 0, 0, 1];
    data.extend_from_slice(d_admin);
    data.extend_from_slice(program);
    data
}

fn propose(d: &Door, program: &[u8]) -> u64 {
    use sha2::{Digest, Sha256};
    let answer: Result<u64, String> = update(
        &d.pic,
        d.canister,
        d.admin,
        "propose_solana_upgrade",
        encode_one(ProposeSolanaUpgradeArgs {
            buffer: base58::encode(&[13; 32]),
            build_hash: Sha256::digest(program).to_vec(),
            source_commit: "abc".to_string(),
        })
        .unwrap(),
    );
    answer.unwrap()
}

#[test]
fn an_upgrade_signs_the_published_build_after_its_delay_and_a_cancel_mid_way_stands() {
    let program = vec![7u8; 200];
    let d = door_on(|a| Chain {
        account: Some(account_of(
            &UPGRADEABLE_LOADER,
            &buffered_build(&a.admin, &program),
        )),
        ..Chain::default()
    });
    let execute = |id: u64| encode_args((id, lifetime())).unwrap();
    let first = propose(&d, &program);
    let early: Result<u64, String> =
        update(&d.pic, d.canister, d.admin, "execute_solana_upgrade", execute(first));
    assert!(early.unwrap_err().contains("delay"));

    d.pic.advance_time(Duration::from_secs(172_801));
    d.pic.tick();

    // the cancel lands while the door's reads and signatures are out
    let before = lines(&d);
    let call = d
        .pic
        .submit_call(d.canister, d.admin, "execute_solana_upgrade", execute(first))
        .unwrap();
    d.pic.tick();
    let cancelled: Result<(), String> = update(
        &d.pic,
        d.canister,
        d.admin,
        "cancel_solana_upgrade",
        encode_one(first).unwrap(),
    );
    cancelled.unwrap();
    let answer: Result<u64, String> =
        decode_one(&d.pic.await_call(call).expect("the door answers")).unwrap();
    assert!(answer.unwrap_err().contains("not the live one"));
    assert_eq!(lines(&d), before, "nothing was recorded, so nothing is broadcast");
    let live: Option<SolanaUpgradeView> =
        query(&d.pic, d.canister, "solana_live_upgrade", encode_args(()).unwrap());
    assert!(live.is_none());

    // a build proposed again waits again, then signs once
    let second = propose(&d, &program);
    d.pic.advance_time(Duration::from_secs(172_801));
    d.pic.tick();
    let id: Result<u64, String> =
        update(&d.pic, d.canister, d.admin, "execute_solana_upgrade", execute(second));
    let tx = signed(&d, id.unwrap());
    assert_eq!(tx.addresses[1], d.a.admin);
    assert!(tx.addresses.contains(&UPGRADEABLE_LOADER));
    let again: Result<u64, String> =
        update(&d.pic, d.canister, d.admin, "execute_solana_upgrade", execute(second));
    assert!(again.unwrap_err().contains("not the live one"));
}
