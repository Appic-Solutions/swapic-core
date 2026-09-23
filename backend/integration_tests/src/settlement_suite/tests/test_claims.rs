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
use settlement_api::types::swap::SwapStatus;
use std::collections::BTreeMap;
use std::time::Duration;
use types::abi::deposited_topic;
use types::{ChainId, GasMode, Rail, TokenAmount, UnixSeconds};

const BASE: u64 = 8453;
const VAULT: &str = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913";
const USDC: &str = "0xaf88d065e77c8cC2239327C5EDb3A432268e5831";
const USER: &str = "0x7551A66653f9a20979ed81835a0b7008EC83401b";
const REFUND: &str = "0x1111111111111111111111111111111111111111";
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
        dst_address: "0xuser".parse().unwrap(),
        // a refund has to be payable, so the quote names an address and not any text
        refund_address: Some(REFUND.parse().unwrap()),
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
        (reads[0], reads[1], reads.len()),
        (HEAD, HEAD + 1 - 345_600, 36),
        "the registration height's window, then the plain lookback from its oldest"
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
        Err(ClaimError::Deposit(DepositError::NoneMatches {
            quote_hash,
            seen: 1
        }))
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
    set_sanctioned(&pic, canister, watcher(), &[REFUND], &["0xuser"]).unwrap();
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
        &[REFUND],
    )
    .unwrap();
    let call = submit_claim(&pic, canister, watcher(), &quote);
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
///
/// Rewritten for fix wave 4 (N4): the claim inside the window finds nothing above the
/// registration height and reads the plain lookback before it refuses.
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

    pic.advance_time(Duration::from_secs(121));
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

/// The block a read asks the provider to start at.
fn from_block(request: &CanisterHttpRequest) -> u64 {
    let filter = params(request, 1);
    let text = filter[0]["fromBlock"].as_str().expect("a fromBlock");
    u64::from_str_radix(text.trim_start_matches("0x"), 16).expect("a hex block number")
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
/// claim's log read starts at the height the store kept and reads one window. A quote the
/// store holds no height for is read from the lookback, which is a day of blocks on the
/// chain whose blocks come fastest: a deposit made while the canister was halted is still
/// in the range.
///
/// Rewritten for fix wave 4 (N2): the lookback is now walked from its oldest window
/// forward, since the deposit that counts is the oldest deep one in range, so the
/// controller's claim reads every window up to the one holding the deposit instead of
/// starting at the newest.
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
    let read = the_read(&pic, quote_hash);
    assert_eq!(
        from_block(&read),
        HEAD,
        "the height the quote was first registered at, not a day of blocks back and not \
         the head a retry saw"
    );
    answer(
        &pic,
        &read,
        HEAD + 5_000,
        vec![deposit_log(quote_hash, HEAD + 1, USDC, USER, AMOUNT.into())],
    );
    assert_eq!(await_claim(&pic, call), Ok(quote_hash));

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
        Some(&(HEAD + 5_000 + 1 - 345_600)),
        "the oldest window of the day-wide lookback is read first"
    );
    assert_eq!(
        reads.len(),
        35,
        "and every window after it, up to the newest, which holds the deposit"
    );
}

/// The block a read asks the provider to end at: the head for the newest window.
fn to_block(request: &CanisterHttpRequest, latest: u64) -> u64 {
    let filter = params(request, 1);
    match filter[0]["toBlock"].as_str().expect("a toBlock") {
        "latest" => latest,
        text => u64::from_str_radix(text.trim_start_matches("0x"), 16).expect("a hex block"),
    }
}

/// Answers one read the way a provider does over the range it names: the head at
/// `latest`, and those of `logs` whose block is inside the range, in the order given.
fn answer_in_range(pic: &PocketIc, request: &CanisterHttpRequest, latest: u64, logs: &[Value]) {
    let (from, to) = (from_block(request), to_block(request, latest));
    let within = logs
        .iter()
        .filter(|log| {
            let text = log["blockNumber"].as_str().expect("a block number");
            let block = u64::from_str_radix(text.trim_start_matches("0x"), 16).unwrap();
            (from..=to).contains(&block)
        })
        .cloned()
        .collect();
    answer(pic, request, latest, within);
}

/// Answers every window a claim reads, one outcall at a time, until it asks for nothing
/// more, and gives back the block each read started at, in the order they were asked.
fn walk_the_reads(pic: &PocketIc, quote_hash: Hash32, latest: u64, logs: &[Value]) -> Vec<u64> {
    let mut froms = Vec::new();
    while !pic.get_canister_http().is_empty() {
        let read = the_read(pic, quote_hash);
        froms.push(from_block(&read));
        answer_in_range(pic, &read, latest, logs);
    }
    froms
}

/// A lookback of two windows and a depth of six on Base, so a test can put a deposit in
/// each window and one of them short of the depth.
fn two_windows_six_deep(pic: &PocketIc, canister: Principal, admin: Principal) {
    use crate::client::settlement::{get_config_full, set_config};
    let config = get_config_full(pic, canister, admin).expect("the controller reads it");
    set_config(
        pic,
        canister,
        admin,
        &Config {
            deposit_lookback_blocks: 20_000,
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
/// older window, although another payer's deposit of the same quote and amount sits in the
/// newer one short of the depth.
#[test]
fn a_deep_deposit_in_an_older_window_claims_behind_a_shallow_one_in_a_newer() {
    let (pic, canister, admin) = setup();
    two_windows_six_deep(&pic, canister, admin);
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
    let shallow = logged_deposit(
        quote_hash,
        HEAD - 2,
        USDC,
        REFUND,
        AMOUNT.into(),
        [0x72; 32],
    );
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
        vec![HEAD + 1 - 20_000],
        "the older window is read first, and the deep deposit in it ends the walk"
    );
    assert!(verify_replay(&pic, canister, Principal::anonymous()));
}

/// Among several deep deposits in range the claim takes the oldest, the first the quote's
/// hash was paid with, wherever the window boundaries fall: across two windows the older
/// window's, and inside one window the lower block's, whatever order the provider lists
/// them in.
#[test]
fn the_oldest_of_several_deep_deposits_is_the_one_claimed() {
    let (pic, canister, admin) = setup();
    two_windows_six_deep(&pic, canister, admin);
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
        REFUND,
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
        REFUND,
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
#[test]
fn a_stale_reading_registers_no_height_and_the_quote_stays_claimable() {
    let (pic, canister, admin) = setup();
    lookback_of(&pic, canister, admin, 20_000);
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
        vec![HEAD + 1_000 + 1 - 20_000, HEAD + 1_000 + 1 - 10_000],
        "no height, so the plain lookback from its oldest window, and not the stale head"
    );
}

/// The registration height is the watcher's head, and a watcher's head can run ahead of
/// the chain. A quote registered while such a head is fresh keeps it, and the user's
/// deposit lands below it; by the claim the chain has caught up past it, so nothing about
/// the claim's own reading looks wrong. The claim reads from the height, finds nothing at
/// all above it, and reads the plain lookback before refusing, so it claims the deposit
/// rather than stranding it.
#[test]
fn a_watcher_head_ahead_of_the_chain_cannot_strand_a_quote() {
    let (pic, canister, admin) = setup();
    lookback_of(&pic, canister, admin, 100_000);
    push_head(&pic, canister, HEAD + 50_000);
    let quote = quote(44);
    let quote_hash = registered(&pic, canister, &quote);
    let deposit = deposit_log(quote_hash, HEAD + 10, USDC, USER, AMOUNT.into());
    push_head(&pic, canister, HEAD + 60_000);
    let call = submit_claim_unregistered(&pic, canister, watcher(), &wire(&quote));
    let reads = walk_the_reads(&pic, quote_hash, HEAD + 60_000, &[deposit]);
    assert_eq!(await_claim(&pic, call), Ok(quote_hash));
    assert_eq!(
        reads[..3],
        [HEAD + 50_000, HEAD + 50_001, HEAD + 60_000 + 1 - 100_000],
        "from the height the quote was registered at, then, with nothing above it, the \
         plain lookback from its oldest window"
    );
    assert!(verify_replay(&pic, canister, Principal::anonymous()));
}
