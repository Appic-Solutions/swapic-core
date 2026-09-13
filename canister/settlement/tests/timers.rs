mod common;

use candid::{decode_one, encode_args, encode_one, Principal};
use pocket_ic::PocketIc;
use settlement::events::{Event, EventEnvelope, Hash32};
use settlement::quote::{quote_bytes, quote_hash, GasMode, Quote};
use settlement::state::{SwapState, SwapStatus};
use std::time::Duration;

fn quoter() -> Principal {
    Principal::from_slice(&[2; 29])
}

fn watcher() -> Principal {
    Principal::from_slice(&[3; 29])
}

fn stranger() -> Principal {
    Principal::from_slice(&[9; 29])
}

/// Seconds on the pic clock, which is the clock the sweep reads.
fn now_s(pic: &PocketIc) -> u64 {
    pic.get_time().as_nanos_since_unix_epoch() / 1_000_000_000
}

/// Move the clock, then give the canister enough rounds for the due timer to run.
fn advance(pic: &PocketIc, seconds: u64) {
    pic.advance_time(Duration::from_secs(seconds));
    for _ in 0..3 {
        pic.tick();
    }
}

fn set_roles(pic: &PocketIc, canister: Principal, sender: Principal) -> Result<(), String> {
    let raw = pic
        .update_call(
            canister,
            sender,
            "set_roles",
            encode_args((quoter(), watcher())).unwrap(),
        )
        .unwrap();
    decode_one(&raw).unwrap()
}

fn set_halted(
    pic: &PocketIc,
    canister: Principal,
    sender: Principal,
    halted: bool,
) -> Result<(), String> {
    let raw = pic
        .update_call(canister, sender, "set_halted", encode_one(halted).unwrap())
        .unwrap();
    decode_one(&raw).unwrap()
}

fn halted(pic: &PocketIc, canister: Principal) -> bool {
    common::query(pic, canister, stranger(), "halted", encode_one(()).unwrap())
}

fn register_quote(pic: &PocketIc, canister: Principal, quote: &Quote) -> Result<Hash32, String> {
    let raw = pic
        .update_call(
            canister,
            quoter(),
            "register_quote",
            encode_one(quote).unwrap(),
        )
        .unwrap();
    decode_one(&raw).unwrap()
}

fn get_pending(pic: &PocketIc, canister: Principal, hash: Hash32) -> Option<Quote> {
    let answer: Result<Option<Quote>, String> = common::query(
        pic,
        canister,
        quoter(),
        "get_pending",
        encode_one(hash).unwrap(),
    );
    answer.expect("the quoter may read the store")
}

fn get_swap(pic: &PocketIc, canister: Principal, hash: Hash32) -> Option<SwapState> {
    common::query(
        pic,
        canister,
        stranger(),
        "get_swap",
        encode_one(hash).unwrap(),
    )
}

fn events(pic: &PocketIc, canister: Principal) -> Vec<EventEnvelope> {
    common::query(
        pic,
        canister,
        stranger(),
        "events_page",
        encode_args((0u64, 100u64)).unwrap(),
    )
}

fn verifies(pic: &PocketIc, canister: Principal) -> bool {
    let chain: bool = common::query(
        pic,
        canister,
        stranger(),
        "verify_chain",
        encode_one(()).unwrap(),
    );
    let replay: bool = common::query(
        pic,
        canister,
        stranger(),
        "verify_replay",
        encode_one(()).unwrap(),
    );
    chain && replay
}

fn quote_expiring_in(pic: &PocketIc, ttl_s: u64, auto_refund: bool, nonce: u64) -> Quote {
    Quote {
        version: 1,
        src_chain: 8453,
        src_token: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".into(),
        amount_in: 25_000_000,
        dst_chain: 42161,
        dst_token: "0xaf88d065e77c8cC2239327C5EDb3A432268e5831".into(),
        expected_out: 24_990_000,
        min_out: 24_900_000,
        dst_address: "0x7551A66653f9a20979ed81835a0b7008EC83401b".into(),
        refund_address: None,
        auto_refund,
        gas_mode: GasMode::Gasless,
        rail: "cctp_v2_fast".into(),
        expires_at_s: now_s(pic) + ttl_s,
        nonce,
    }
}

/// A swap parked on a user decision, with a real quote in `quote_bytes` so the sweep can
/// read its `auto_refund`. Money first: `FundsReceived` is what creates the swap.
fn waiting_swap(
    pic: &PocketIc,
    canister: Principal,
    admin: Principal,
    auto_refund: bool,
    nonce: u64,
) -> Hash32 {
    let q = quote_expiring_in(pic, 60, auto_refund, nonce);
    let hash = quote_hash(&q);
    common::append(
        pic,
        canister,
        admin,
        &Event::FundsReceived {
            quote_hash: hash,
            quote_bytes: quote_bytes(&q),
            chain_id: q.src_chain,
            token: q.src_token.clone(),
            amount: q.amount_in,
            tx_ref: format!("0xdeposit{nonce}"),
        },
    )
    .expect("money first");
    common::append(
        pic,
        canister,
        admin,
        &Event::DecisionRequired {
            quote_hash: hash,
            reason: "slippage".into(),
        },
    )
    .expect("the swap pauses on the user");
    hash
}

/// A quote nobody paid must not hold its slot forever. The window is the expiry plus the
/// `permit_deadline_s` a signed permit stays good for, and the sweep runs on its own.
#[test]
fn expired_pending_quote_is_swept() {
    let (pic, canister, admin) = common::setup();
    set_roles(&pic, canister, admin).unwrap();
    let q = quote_expiring_in(&pic, 10, true, 1);
    let hash = register_quote(&pic, canister, &q).unwrap();
    assert_eq!(get_pending(&pic, canister, hash), Some(q));

    // expired, but inside the permit window: a deposit signed against it can still land
    advance(&pic, 60);
    assert!(
        get_pending(&pic, canister, hash).is_some(),
        "the permit window is still open"
    );

    advance(&pic, 300);
    assert_eq!(get_pending(&pic, canister, hash), None);
}

/// The dual refund policy, end to end through the timer: a quote that asked for an
/// automatic refund gets one when the user never answers, and a quote that did not keeps
/// waiting for a human.
#[test]
fn decision_timeout_auto_refunds_only_the_quotes_that_asked_for_it() {
    let (pic, canister, admin) = common::setup();
    let auto = waiting_swap(&pic, canister, admin, true, 1);
    let manual = waiting_swap(&pic, canister, admin, false, 2);
    let before = events(&pic, canister).len();

    // the default decision_timeout_min is 30
    advance(&pic, 31 * 60);

    assert_eq!(
        get_swap(&pic, canister, auto).unwrap().status,
        SwapStatus::Refunding
    );
    assert_eq!(
        get_swap(&pic, canister, manual).unwrap().status,
        SwapStatus::WaitingForUser,
        "auto_refund false waits for a human"
    );

    let logged = events(&pic, canister);
    assert_eq!(logged.len(), before + 1, "one timeout, one event");
    assert_eq!(
        logged.last().unwrap().event,
        Event::RefundStarted {
            quote_hash: auto,
            reason: "decision timeout".to_string(),
        }
    );
    assert!(
        verifies(&pic, canister),
        "the sweep appends like everyone else"
    );

    // and it does not fire twice: the swap is Refunding now, which the guard refuses
    advance(&pic, 31 * 60);
    assert_eq!(events(&pic, canister).len(), before + 1);
}

/// The audit timer is wired too, not only the expiry one, and a healthy canister survives
/// it: a false halt would freeze every money path, so this is the more dangerous direction.
/// The log it audits is one the sweep itself appended to.
#[test]
fn the_replay_audit_runs_on_its_own_and_halts_nothing_healthy() {
    let (pic, canister, admin) = common::setup();
    let swap = waiting_swap(&pic, canister, admin, true, 1);

    // the default replay_audit_interval_s is 21_600
    advance(&pic, 21_660);

    assert!(!halted(&pic, canister), "a healthy canister is not halted");
    assert!(verifies(&pic, canister));
    // six hours is also many expiry passes, so the refund happened and the audit read it
    assert_eq!(
        get_swap(&pic, canister, swap).unwrap().status,
        SwapStatus::Refunding
    );
}

/// A halted canister has a log and a state that disagree, so it appends nothing. Dropping
/// stale quotes is pre-money hygiene and keeps running.
#[test]
fn a_halted_canister_starts_no_refund_and_still_drops_stale_quotes() {
    let (pic, canister, admin) = common::setup();
    set_roles(&pic, canister, admin).unwrap();
    let q = quote_expiring_in(&pic, 10, true, 1);
    let pending = register_quote(&pic, canister, &q).unwrap();
    let swap = waiting_swap(&pic, canister, admin, true, 2);
    set_halted(&pic, canister, admin, true).unwrap();

    advance(&pic, 31 * 60);

    assert_eq!(
        get_swap(&pic, canister, swap).unwrap().status,
        SwapStatus::WaitingForUser,
        "a halted canister writes no event"
    );
    assert_eq!(get_pending(&pic, canister, pending), None);

    // and the refund the halt held back happens on the next sweep after a human clears it
    set_halted(&pic, canister, admin, false).unwrap();
    advance(&pic, 60);
    assert_eq!(
        get_swap(&pic, canister, swap).unwrap().status,
        SwapStatus::Refunding
    );
}

#[test]
fn set_halted_refuses_a_stranger() {
    let (pic, canister, admin) = common::setup();
    assert!(set_halted(&pic, canister, stranger(), true).is_err());
    assert!(set_halted(&pic, canister, quoter(), true).is_err());
    assert!(!halted(&pic, canister));
    set_halted(&pic, canister, admin, true).unwrap();
    assert!(halted(&pic, canister));
}

/// An upgrade is the first thing an operator reaches for, so the flag is stable: a halt
/// outlives the redeploy and only a deliberate call clears it.
#[test]
fn the_halt_flag_survives_an_upgrade() {
    let (pic, canister, admin) = common::setup();
    set_halted(&pic, canister, admin, true).unwrap();

    pic.upgrade_canister(
        canister,
        common::wasm(),
        encode_one(()).unwrap(),
        Some(admin),
    )
    .unwrap();

    assert!(halted(&pic, canister), "a redeploy is not an investigation");
    set_halted(&pic, canister, admin, false).unwrap();
    assert!(!halted(&pic, canister));
}

/// What Plan 3 and the indexer read a swap out of.
#[test]
fn get_swap_answers_from_the_folded_state() {
    let (pic, canister, admin) = common::setup();
    assert_eq!(get_swap(&pic, canister, [0; 32]), None, "no such swap");

    let hash = waiting_swap(&pic, canister, admin, true, 1);
    let swap = get_swap(&pic, canister, hash).expect("the swap exists");
    assert_eq!(swap.status, SwapStatus::WaitingForUser);
    assert_eq!(swap.amount_in, 25_000_000);
    assert_eq!(swap.src_chain, 8453);
    assert!(swap.waiting_since_ns.is_some(), "the clock is running");
}
