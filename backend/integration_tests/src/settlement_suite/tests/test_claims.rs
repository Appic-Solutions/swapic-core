//! The entry doors from outside: the claim that creates a swap from a deposit the chain
//! holds, the gasless pull that makes such a deposit, and the attestation inbox.
//!
//! The claim's one outcall is mocked here the way the outbox tests mock theirs: the test
//! submits the call, reads the pending request, and answers it with a head block and the
//! vault's logs.

use crate::client::settlement::{
    claim_swap, derive_evm_address, event_count, events_page, get_swap, push_attestation,
    push_chain_data, push_eco_intent, register_quote, set_halted, set_sanctioned,
    start_gasless_pull, verify_replay,
};
use crate::settlement_suite::init::{empty_canister, install, quoter, watcher};
use candid::{encode_one, Nat, Principal};
use pocket_ic::common::rest::{
    CanisterHttpReply, CanisterHttpRequest, CanisterHttpResponse, MockCanisterHttpResponse,
    RawMessageId,
};
use pocket_ic::{PocketIc, PocketIcBuilder};
use serde_json::{json, Value};
use settlement_api::types::chain_data::ChainData;
use settlement_api::types::config::Config;
use settlement_api::types::entry::{
    ClaimError, DepositError, PermitSig, PullError, PushAttestationError,
};
use settlement_api::types::errors::GuardError;
use settlement_api::types::events::{Event, EventType, Hash32, TxPurpose};
use settlement_api::types::init::InitArg;
use settlement_api::types::quote::Quote;
use settlement_api::types::swap::SwapStatus;
use std::collections::BTreeMap;
use std::time::Duration;
use types::abi::deposited_topic;
use types::{ChainId, GasMode, Rail, TokenAmount, UnixSeconds};

const BASE: u64 = 8453;
const VAULT: &str = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913";
const USDC: &str = "0xaf88d065e77c8cC2239327C5EDb3A432268e5831";
const USER: &str = "0x7551A66653f9a20979ed81835a0b7008EC83401b";
const HEAD: u64 = 19_000_000;
const AMOUNT: u32 = 25_000_000;

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
        dst_address: "0xuser".parse().unwrap(),
        refund_address: Some("0xrefund".parse().unwrap()),
        auto_refund: true,
        gas_mode: GasMode::Legacy,
        rail: Rail::CctpV2Fast,
        expires_at: UnixSeconds::new(1_800_000_000),
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
    push_reading(&pic, canister);
    (pic, canister, admin)
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

/// Submits a claim and gives the canister the rounds it needs to reach its outcall.
fn submit_claim(
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
    let griefer = "0x1111111111111111111111111111111111111111";
    logged_deposit(quote_hash, block, USDC, griefer, amount, [0x66; 32])
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

/// Answers the claim's one outcall: the head at `latest`, and `logs` for the quote.
fn answer(pic: &PocketIc, request: &CanisterHttpRequest, latest: u64, logs: Vec<Value>) {
    let body = json!([
        {"jsonrpc": "2.0", "id": 0, "result": format!("0x{latest:x}")},
        {"jsonrpc": "2.0", "id": 1, "result": logs},
    ]);
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
    for _ in 0..4 {
        pic.tick();
    }
}

/// The one pending outcall, which must be the claim's read: the head and the vault's logs
/// for the quote, in one batch.
fn the_read(pic: &PocketIc, quote_hash: Hash32) -> CanisterHttpRequest {
    let pending = pic.get_canister_http();
    assert_eq!(pending.len(), 1, "a claim buys one outcall");
    let request = pending.into_iter().next().unwrap();
    assert_eq!(methods(&request), vec!["eth_blockNumber", "eth_getLogs"]);
    let filter = params(&request, 1);
    assert_eq!(filter[0]["address"], json!(VAULT.to_ascii_lowercase()));
    assert_eq!(
        filter[0]["topics"][1],
        json!(format!("0x{}", hex::encode(quote_hash))),
        "the read is for this quote"
    );
    request
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

    let call = submit_claim(&pic, canister, watcher(), &wire(&quote));
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
#[test]
fn no_deposit_means_nothing_stored() {
    let (pic, canister, _admin) = setup();
    let quote = quote(2);
    let quote_hash = swap_id(&quote);
    let before = count(&pic, canister);

    let call = submit_claim(&pic, canister, watcher(), &wire(&quote));
    answer(&pic, &the_read(&pic, quote_hash), HEAD, vec![]);
    assert_eq!(
        await_claim(&pic, call),
        Err(ClaimError::Deposit(DepositError::NotFound { quote_hash }))
    );
    assert_eq!(count(&pic, canister), before, "nothing stored");
    assert_eq!(
        get_swap(&pic, canister, Principal::anonymous(), quote_hash),
        None
    );

    // the deposit is there, in a block the head has not reached: not yet
    let call = submit_claim(&pic, canister, watcher(), &wire(&quote));
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
    let call = submit_claim(&pic, canister, watcher(), &wire(&quote));
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
#[test]
fn a_deposit_of_another_token_or_amount_is_refused_and_nothing_stored() {
    let (pic, canister, _admin) = setup();
    let quote = quote(3);
    let quote_hash = swap_id(&quote);
    let before = count(&pic, canister);

    let call = submit_claim(&pic, canister, watcher(), &wire(&quote));
    answer(
        &pic,
        &the_read(&pic, quote_hash),
        HEAD,
        vec![deposit_log(quote_hash, HEAD, USER, USER, AMOUNT.into())],
    );
    assert_eq!(
        await_claim(&pic, call),
        Err(ClaimError::Deposit(DepositError::NoneMatches {
            quote_hash,
            seen: 1
        }))
    );

    let call = submit_claim(&pic, canister, watcher(), &wire(&quote));
    answer(
        &pic,
        &the_read(&pic, quote_hash),
        HEAD,
        vec![deposit_log(
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

    let call = submit_claim(&pic, canister, watcher(), &wire(&quote));
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
    let call = submit_claim(&pic, canister, watcher(), &wire(&behind_forty));
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
#[test]
fn a_sanctioned_party_is_refused_and_the_destination_before_any_outcall() {
    let (pic, canister, _admin) = setup();
    let quote = quote(4);
    let quote_hash = swap_id(&quote);
    let before = count(&pic, canister);

    set_sanctioned(&pic, canister, watcher(), &["0xuser"], &[]).unwrap();
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
    set_sanctioned(&pic, canister, watcher(), &["0xrefund"], &["0xuser"]).unwrap();
    assert_eq!(
        claim_swap(&pic, canister, watcher(), &wire(&quote)),
        Err(ClaimError::Sanctioned {
            party: "refund_address".to_string()
        })
    );
    assert!(pic.get_canister_http().is_empty());

    // the payer is on the chain, so the read happens, and then the refusal: the payer is
    // an EVM address, so its spelling does not matter
    set_sanctioned(
        &pic,
        canister,
        watcher(),
        &[&USER.to_ascii_lowercase()],
        &["0xrefund"],
    )
    .unwrap();
    let call = submit_claim(&pic, canister, watcher(), &wire(&quote));
    answer(
        &pic,
        &the_read(&pic, quote_hash),
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

/// Rule A8: two claims for one quote that are both in flight buy one outcall. The second
/// finds the first's marker and is refused at once, and the first goes on to create the
/// swap.
#[test]
fn concurrent_claims_buy_one_outcall() {
    let (pic, canister, _admin) = setup();
    let quote = quote(5);
    let quote_hash = swap_id(&quote);

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
    let refused = await_claim(&pic, second);
    assert!(
        matches!(refused, Err(ClaimError::InFlight { .. })),
        "the second claim is refused by the first's marker: {refused:?}"
    );
    answer(
        &pic,
        &read,
        HEAD,
        vec![deposit_log(quote_hash, HEAD, USDC, USER, AMOUNT.into())],
    );
    assert_eq!(await_claim(&pic, first), Ok(quote_hash));

    // and the marker went with the message chain: the quote is not held after it
    assert_eq!(
        claim_swap(&pic, canister, watcher(), &wire(&quote)),
        Err(ClaimError::SwapExists(quote_hash)),
        "refused by the swap now, not by a marker"
    );
}

/// A quote nobody can pay any more is not claimed: past its expiry plus the permit
/// window, the claim is refused before any outcall. Inside the window it still is.
#[test]
fn an_expired_quote_is_refused_before_any_outcall() {
    let (pic, canister, _admin) = setup();
    let quote = types::Quote {
        expires_at: UnixSeconds::new(pic.get_time().as_nanos_since_unix_epoch() / 1_000_000_000),
        ..quote(6)
    };
    // the default permit window is two minutes: a minute in, the quote is still claimable
    pic.advance_time(Duration::from_secs(60));
    push_reading(&pic, canister);
    let call = submit_claim(&pic, canister, watcher(), &wire(&quote));
    answer(&pic, &the_read(&pic, swap_id(&quote)), HEAD, vec![]);
    assert!(matches!(
        await_claim(&pic, call),
        Err(ClaimError::Deposit(DepositError::NotFound { .. }))
    ));

    pic.advance_time(Duration::from_secs(61));
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
        Err(ClaimError::Guard(GuardError::CallerNotQuoterOrWatcher))
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

    let call = submit_claim(&pic, canister, watcher(), &wire(&quote));
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
    let call = submit_claim(&pic, canister, watcher(), &wire(&quote));
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

fn permit(quote: &types::Quote) -> PermitSig {
    PermitSig {
        token: USDC.to_string(),
        owner: USER.to_string(),
        amount: quote.amount_in.into(),
        deadline_s: 1_800_000_100,
        v: 28,
        r: [0x22; 32],
        s: [0x33; 32],
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
/// `pullWithPermit`. A second pull while the first is on its way is refused, and no swap
/// exists until a claim verifies the deposit the pull made.
#[test]
fn a_pending_gasless_quote_is_pulled_through_the_send_path() {
    let (pic, canister, _admin) = setup_with_keys();
    let quote = types::Quote {
        gas_mode: GasMode::Gasless,
        ..live_quote(&pic, 11)
    };
    let quote_hash = swap_id(&quote);
    register_quote(&pic, canister, quoter(), &wire(&quote)).expect("the quoter registers");

    let wrong_amount = PermitSig {
        amount: Nat::from(1_u8),
        ..permit(&quote)
    };
    assert_eq!(
        start_gasless_pull(&pic, canister, quoter(), quote_hash, &wrong_amount),
        Err(PullError::PermitMismatch {
            field: "amount".to_string()
        })
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
        hex::encode(&signed[0]).contains("0263549d"),
        "the bytes carry pullWithPermit"
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

/// The Eco intent inbox is the watcher's, holds one intent per known swap on the Eco rail,
/// and refuses an intent for a swap on another rail, so no CCTP swap can be steered onto
/// a publish.
#[test]
fn push_eco_intent_is_the_watchers_and_needs_a_known_eco_swap() {
    use settlement_api::types::entry::{EcoIntent, PushEcoIntentError};
    use settlement_api::types::events::EvmAddressError;
    let (pic, canister, _admin) = setup();
    let intent = EcoIntent {
        destination_chain: BASE,
        route: vec![0xde, 0xad, 0xbe, 0xef],
        deadline_s: 1_800_000_500,
        prover: "0xeC00008537c1F26E739486BCFCC818d81234d5aD".to_string(),
    };

    // a swap on CCTP, and one on Eco, both claimed
    let cctp = quote(12);
    let eco = types::Quote {
        rail: Rail::Eco,
        ..quote(13)
    };
    for quote in [&cctp, &eco] {
        let quote_hash = swap_id(quote);
        assert_eq!(
            push_eco_intent(&pic, canister, watcher(), quote_hash, &intent),
            Err(PushEcoIntentError::UnknownSwap(quote_hash)),
            "no swap, no inbox slot"
        );
        let call = submit_claim(&pic, canister, watcher(), &wire(quote));
        answer(
            &pic,
            &the_read(&pic, quote_hash),
            HEAD,
            vec![deposit_log(quote_hash, HEAD, USDC, USER, AMOUNT.into())],
        );
        assert_eq!(await_claim(&pic, call), Ok(quote_hash));
    }

    assert_eq!(
        push_eco_intent(
            &pic,
            canister,
            Principal::from_slice(&[9; 29]),
            swap_id(&eco),
            &intent
        ),
        Err(PushEcoIntentError::Guard(GuardError::CallerNotRole(
            settlement_api::types::errors::Role::Watcher
        )))
    );
    assert_eq!(
        push_eco_intent(&pic, canister, watcher(), swap_id(&cctp), &intent),
        Err(PushEcoIntentError::NotAnEcoSwap(swap_id(&cctp)))
    );
    assert_eq!(
        push_eco_intent(&pic, canister, watcher(), swap_id(&eco), &intent),
        Ok(())
    );
    assert_eq!(
        push_eco_intent(&pic, canister, watcher(), swap_id(&eco), &intent),
        Ok(()),
        "idempotent"
    );
    let bad_prover = EcoIntent {
        prover: "prover".to_string(),
        ..intent.clone()
    };
    assert_eq!(
        push_eco_intent(&pic, canister, watcher(), swap_id(&eco), &bad_prover),
        Err(PushEcoIntentError::ProverNotAnAddress {
            reason: EvmAddressError::NoPrefix
        })
    );
    let long_route = EcoIntent {
        route: vec![0; 8_193],
        ..intent
    };
    assert_eq!(
        push_eco_intent(&pic, canister, watcher(), swap_id(&eco), &long_route),
        Err(PushEcoIntentError::RouteTooLong {
            len: 8_193,
            cap: 8_192
        })
    );
}
