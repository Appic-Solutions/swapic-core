use crate::client::settlement::{
    self, append, events_page, set_config, set_halted, test_skew_state,
};
use crate::settlement_suite::init::{quoter, setup};
use crate::wasms;
use candid::{encode_one, Nat, Principal};
use pocket_ic::PocketIc;
use settlement_api::types::config::Config;
use settlement_api::types::errors::{GuardError, RegisterQuoteError};
use settlement_api::types::events::{Event, EventType, Hash32};
use settlement_api::types::quote::{GasMode, Quote};
use settlement_api::types::swap::{Swap, SwapStatus};
use std::time::Duration;

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

/// The test-only door that moves the fold's chain head off the log's, which is exactly
/// what the replay audit halts on.
fn skew_state(pic: &PocketIc, canister: Principal, sender: Principal) -> Result<(), GuardError> {
    test_skew_state(pic, canister, sender)
}

fn halted(pic: &PocketIc, canister: Principal) -> bool {
    settlement::halted(pic, canister, stranger())
}

fn register_quote(
    pic: &PocketIc,
    canister: Principal,
    quote: &Quote,
) -> Result<Hash32, RegisterQuoteError> {
    settlement::register_quote(pic, canister, quoter(), quote)
}

fn get_pending(pic: &PocketIc, canister: Principal, hash: Hash32) -> Option<Quote> {
    let answer: Result<Option<Quote>, GuardError> =
        settlement::get_pending(pic, canister, quoter(), hash);
    answer.expect("the quoter may read the store")
}

fn get_swap(pic: &PocketIc, canister: Principal, hash: Hash32) -> Option<Swap> {
    settlement::get_swap(pic, canister, stranger(), hash)
}

fn events(pic: &PocketIc, canister: Principal) -> Vec<Event> {
    events_page(pic, canister, stranger(), 0, 100)
}

fn verifies(pic: &PocketIc, canister: Principal) -> bool {
    let chain: bool = settlement::verify_chain(pic, canister, stranger());
    let replay: bool = settlement::verify_replay(pic, canister, stranger());
    chain && replay
}

fn quote_expiring_in(pic: &PocketIc, ttl_s: u64, auto_refund: bool, nonce: u64) -> Quote {
    Quote {
        version: 1,
        src_chain: 8453,
        src_token: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".into(),
        amount_in: Nat::from(25_000_000_u32),
        dst_chain: 42161,
        dst_token: "0xaf88d065e77c8cC2239327C5EDb3A432268e5831".into(),
        expected_out: Nat::from(24_990_000_u32),
        min_out: Nat::from(24_900_000_u32),
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
    let q = types::Quote::try_from(quote_expiring_in(pic, 60, auto_refund, nonce))
        .expect("a valid quote");
    let hash = q.hash().expect("a valid quote has an id").into_bytes();
    append(
        pic,
        canister,
        admin,
        &EventType::FundsReceived {
            quote_hash: hash,
            quote_bytes: q.canonical_bytes().expect("a valid quote has a preimage"),
            chain_id: q.src_chain.get(),
            token: q.src_token.to_string(),
            amount: q.amount_in.into(),
            tx_ref: format!("0xdeposit{nonce}"),
        },
    )
    .expect("money first");
    append(
        pic,
        canister,
        admin,
        &EventType::DecisionRequired {
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
    let (pic, canister, _) = setup();
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
    let (pic, canister, admin) = setup();
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
        logged.last().unwrap().payload,
        EventType::RefundStarted {
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
    let (pic, canister, admin) = setup();
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
    let (pic, canister, admin) = setup();
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

/// `set_config` rewires both timers on the spot, and each call replaces the timers the one
/// before it set. Both intervals move on every call and every generation runs at its own
/// cadence, so a timer a restart missed would show up as a sweep or an audit at a cadence
/// nobody configured any more.
#[test]
fn set_config_rewires_the_timers_and_leaves_no_stale_one_running() {
    let (pic, canister, admin) = setup();
    // init wired an expiry pass every 60s and an audit every 21_600s
    let fast = Config {
        expiry_check_interval_s: 120,
        replay_audit_interval_s: 60,
        ..Config::default()
    };
    set_config(&pic, canister, admin, &fast).unwrap();
    let slow = Config {
        expiry_check_interval_s: 3_600,
        replay_audit_interval_s: 43_200,
        ..Config::default()
    };
    set_config(&pic, canister, admin, &slow).unwrap();

    // a quote any sweep past 130s would drop, and a divergence any audit would halt on
    let q = quote_expiring_in(&pic, 10, true, 1);
    let pending = register_quote(&pic, canister, &q).unwrap();
    skew_state(&pic, canister, admin).unwrap();

    advance(&pic, 600);
    assert!(
        get_pending(&pic, canister, pending).is_some(),
        "no expiry timer is left from init (60s) or from the first set_config (120s)"
    );
    assert!(
        !halted(&pic, canister),
        "and no 60s audit timer from the first set_config"
    );

    // the new expiry cadence is live without an upgrade
    advance(&pic, 3_100);
    assert_eq!(get_pending(&pic, canister, pending), None);

    advance(&pic, 18_000);
    assert!(!halted(&pic, canister), "init's 21_600s audit is gone too");

    advance(&pic, 21_600);
    assert!(halted(&pic, canister), "and the 43_200s audit is live");
}

/// A config change that moves no interval must not restart the timers, or every such
/// edit would push the next audit a whole interval out.
#[test]
fn set_config_without_an_interval_change_keeps_the_audit_on_schedule() {
    let (pic, canister, admin) = setup();
    // init wired the audit for 21_600s from now
    advance(&pic, 21_000);
    set_config(
        &pic,
        canister,
        admin,
        &Config {
            platform_fee_bps: 10,
            ..Config::default()
        },
    )
    .unwrap();
    skew_state(&pic, canister, admin).unwrap();

    // a restarted audit would not be due until about 42_600s
    advance(&pic, 700);
    assert!(halted(&pic, canister), "the audit ran on init's schedule");
}

/// Each timer restarts only for its own interval: an expiry tweak must not push the audit
/// out, or tweaks more frequent than the audit interval would keep it from ever running.
#[test]
fn an_expiry_only_change_keeps_the_audit_on_schedule() {
    let (pic, canister, admin) = setup();
    // init wired the audit for 21_600s from now
    advance(&pic, 21_000);
    set_config(
        &pic,
        canister,
        admin,
        &Config {
            expiry_check_interval_s: 120,
            ..Config::default()
        },
    )
    .unwrap();
    skew_state(&pic, canister, admin).unwrap();

    // a restarted audit would not be due until about 42_600s
    advance(&pic, 700);
    assert!(halted(&pic, canister), "the audit ran on init's schedule");
}

/// The mirror: an audit-only change takes effect at once and leaves the expiry timer on
/// the schedule it already had.
#[test]
fn an_audit_only_change_takes_effect_and_keeps_the_expiry_on_schedule() {
    let (pic, canister, admin) = setup();
    let slow_sweep = Config {
        expiry_check_interval_s: 600,
        ..Config::default()
    };
    set_config(&pic, canister, admin, &slow_sweep).unwrap();
    // droppable by any sweep past 130s, and the next one is due at 600s
    let q = quote_expiring_in(&pic, 10, true, 1);
    let pending = register_quote(&pic, canister, &q).unwrap();

    advance(&pic, 500);
    set_config(
        &pic,
        canister,
        admin,
        &Config {
            replay_audit_interval_s: 120,
            ..slow_sweep
        },
    )
    .unwrap();

    // a restarted sweep would not be due until about 1_100s
    advance(&pic, 150);
    assert_eq!(
        get_pending(&pic, canister, pending),
        None,
        "the sweep ran on its own schedule"
    );

    // init's audit is not due for hours, so a halt now is the new 120s audit
    skew_state(&pic, canister, admin).unwrap();
    advance(&pic, 130);
    assert!(halted(&pic, canister), "the new audit interval is live");
}

/// The deep check is an ops action, not a timer's: a controller runs it over the whole log
/// and it answers what it compared. A divergence it finds halts the canister, and a
/// stranger cannot run it at all.
#[test]
fn audit_replay_is_the_controllers_deep_check_and_halts_on_a_divergence() {
    let (pic, canister, admin) = setup();
    waiting_swap(&pic, canister, admin, true, 1);
    let len = settlement::event_count(&pic, canister, stranger());

    assert_eq!(
        settlement::audit_replay(&pic, canister, stranger(), 0, len),
        Err(GuardError::NotController),
        "the deep check is controller-only"
    );
    assert_eq!(
        settlement::audit_replay(&pic, canister, quoter(), 0, len),
        Err(GuardError::NotController)
    );

    let page = settlement::audit_replay(&pic, canister, admin, 0, len).expect("a controller may");
    assert_eq!((page.folded, page.log_len), (len, len));
    assert!(page.compared && page.matches && !page.halted);
    assert!(!halted(&pic, canister));

    // the fold's head moved off the log's, which is the divergence the comparison sees
    skew_state(&pic, canister, admin).unwrap();
    let page = settlement::audit_replay(&pic, canister, admin, 0, len).expect("a controller may");
    assert!(page.compared && !page.matches, "{page:?}");
    assert!(page.halted && halted(&pic, canister));
}

#[test]
fn set_halted_refuses_a_stranger() {
    let (pic, canister, admin) = setup();
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
    let (pic, canister, admin) = setup();
    set_halted(&pic, canister, admin, true).unwrap();

    pic.upgrade_canister(
        canister,
        wasms::settlement(),
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
    let (pic, canister, admin) = setup();
    assert_eq!(get_swap(&pic, canister, [0; 32]), None, "no such swap");

    let hash = waiting_swap(&pic, canister, admin, true, 1);
    let swap = get_swap(&pic, canister, hash).expect("the swap exists");
    assert_eq!(swap.status, SwapStatus::WaitingForUser);
    assert_eq!(swap.amount_in, Nat::from(25_000_000_u32));
    assert_eq!(swap.src_chain, 8453);
    assert!(swap.waiting_since_ns.is_some(), "the clock is running");
}
