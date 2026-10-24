use crate::client::settlement::{self, append, set_config, set_halted, test_skew_state};
use crate::settlement_suite::init::{quoter, rail_config, setup_with_rail_usdc, upgrade};
use candid::Principal;
use pocket_ic::PocketIc;
use settlement_api::types::config::Config;
use settlement_api::types::errors::GuardError;
use settlement_api::types::events::{EventType, Hash32};
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

fn verifies(pic: &PocketIc, canister: Principal) -> bool {
    let chain: bool = settlement::verify_chain(pic, canister, stranger());
    let replay: bool = settlement::verify_replay(pic, canister, stranger());
    chain && replay
}

fn quote_expiring_in(pic: &PocketIc, ttl_s: u64, auto_refund: bool, nonce: u64) -> types::Quote {
    types::Quote {
        version: 1,
        src_chain: types::ChainId::BASE,
        src_token: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"
            .parse()
            .unwrap(),
        amount_in: types::TokenAmount::from(25_000_000_u32),
        dst_chain: types::ChainId::ARBITRUM,
        dst_token: "0xaf88d065e77c8cC2239327C5EDb3A432268e5831"
            .parse()
            .unwrap(),
        expected_out: types::TokenAmount::from(24_990_000_u32),
        min_out: types::TokenAmount::from(24_900_000_u32),
        dst_address: "0x7551A66653f9a20979ed81835a0b7008EC83401b"
            .parse()
            .unwrap(),
        // a quote that asks to be refunded on its own says where: without an address the
        // engine can only stop the refund for a human
        refund_address: Some(
            "0x7551A66653f9a20979ed81835a0b7008EC83401b"
                .parse()
                .unwrap(),
        ),
        auto_refund,
        gas_mode: types::GasMode::Gasless,
        rail: types::Rail::CctpV2Fast,
        expires_at: types::UnixSeconds::new(now_s(pic) + ttl_s),
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

/// The audit timer is wired, and a healthy canister survives it: a false halt would freeze
/// every money path, so this is the more dangerous direction.
#[test]
fn the_replay_audit_runs_on_its_own_and_halts_nothing_healthy() {
    let (pic, canister, admin) = setup_with_rail_usdc();
    waiting_swap(&pic, canister, admin, true, 1);

    // the default replay_audit_interval_s is 21_600
    advance(&pic, 21_660);

    assert!(!halted(&pic, canister), "a healthy canister is not halted");
    assert!(verifies(&pic, canister));
}

/// `set_config` rewires the audit timer on the spot, and each call replaces the timer the
/// one before it set. The interval moves on every call and every generation runs at its
/// own cadence, so a timer a restart missed would show up as an audit at a cadence nobody
/// configured any more.
#[test]
fn set_config_rewires_the_timers_and_leaves_no_stale_one_running() {
    let (pic, canister, admin) = setup_with_rail_usdc();
    // init wired an audit every 21_600s
    let fast = Config {
        replay_audit_interval_s: 60,
        ..rail_config()
    };
    set_config(&pic, canister, admin, &fast).unwrap();
    let slow = Config {
        replay_audit_interval_s: 43_200,
        ..rail_config()
    };
    set_config(&pic, canister, admin, &slow).unwrap();

    // a divergence any audit would halt on
    skew_state(&pic, canister, admin).unwrap();

    advance(&pic, 800);
    assert!(
        !halted(&pic, canister),
        "no 60s audit timer from the first set_config"
    );

    advance(&pic, 20_900);
    assert!(!halted(&pic, canister), "init's 21_600s audit is gone too");

    advance(&pic, 21_600);
    assert!(halted(&pic, canister), "and the 43_200s audit is live");
}

/// A config change that moves no interval must not restart the timers, or every such
/// edit would push the next audit a whole interval out.
#[test]
fn set_config_without_an_interval_change_keeps_the_audit_on_schedule() {
    let (pic, canister, admin) = setup_with_rail_usdc();
    // init wired the audit for 21_600s from now
    advance(&pic, 21_000);
    set_config(
        &pic,
        canister,
        admin,
        &Config {
            platform_fee_bps: 10,
            ..rail_config()
        },
    )
    .unwrap();
    skew_state(&pic, canister, admin).unwrap();

    // a restarted audit would not be due until about 42_600s
    advance(&pic, 700);
    assert!(halted(&pic, canister), "the audit ran on init's schedule");
}

/// Each timer restarts only for its own interval: a tweak of the expiry knob (kept in the
/// config, no timer reads it since the sweep went) must not push the audit out, or tweaks
/// more frequent than the audit interval would keep it from ever running.
#[test]
fn an_expiry_only_change_keeps_the_audit_on_schedule() {
    let (pic, canister, admin) = setup_with_rail_usdc();
    // init wired the audit for 21_600s from now
    advance(&pic, 21_000);
    set_config(
        &pic,
        canister,
        admin,
        &Config {
            expiry_check_interval_s: 120,
            ..rail_config()
        },
    )
    .unwrap();
    skew_state(&pic, canister, admin).unwrap();

    // a restarted audit would not be due until about 42_600s
    advance(&pic, 700);
    assert!(halted(&pic, canister), "the audit ran on init's schedule");
}

/// The mirror: an audit-only change takes effect at once.
#[test]
fn an_audit_only_change_takes_effect_at_once() {
    let (pic, canister, admin) = setup_with_rail_usdc();
    advance(&pic, 1_100);
    set_config(
        &pic,
        canister,
        admin,
        &Config {
            replay_audit_interval_s: 120,
            ..rail_config()
        },
    )
    .unwrap();

    // init's audit is not due for hours, so a halt now is the new 120s audit
    skew_state(&pic, canister, admin).unwrap();
    advance(&pic, 130);
    assert!(halted(&pic, canister), "the new audit interval is live");
}

/// The deep check is an ops action, not a timer's: a controller runs it over the whole log,
/// a step at a time, and it answers what it compared. A divergence it finds halts the
/// canister, and a stranger cannot run it at all.
#[test]
fn audit_replay_step_is_the_controllers_deep_check_and_halts_on_a_divergence() {
    let (pic, canister, admin) = setup_with_rail_usdc();
    waiting_swap(&pic, canister, admin, true, 1);
    let len = settlement::event_count(&pic, canister, stranger());

    assert_eq!(
        settlement::audit_replay_step(&pic, canister, stranger(), len),
        Err(GuardError::NotController),
        "the deep check is controller-only"
    );
    assert_eq!(
        settlement::audit_replay_step(&pic, canister, quoter(), len),
        Err(GuardError::NotController)
    );

    // a step short of the head reports how far it got and condemns nothing
    let first = settlement::audit_replay_step(&pic, canister, admin, 1).expect("a controller may");
    assert_eq!(
        (first.folded_so_far, first.remaining, first.finished),
        (1, len - 1, false)
    );
    assert!(!first.halted && !halted(&pic, canister));
    // the next step carries on from there and finishes
    let last = settlement::audit_replay_step(&pic, canister, admin, len).expect("a controller may");
    assert_eq!((last.folded_so_far, last.remaining), (len, 0));
    assert!(last.finished && last.matches && !last.halted, "{last:?}");
    assert!(!halted(&pic, canister));

    // the fold's head moved off the log's, which is the divergence the comparison sees
    skew_state(&pic, canister, admin).unwrap();
    let step = settlement::audit_replay_step(&pic, canister, admin, len).expect("a controller may");
    assert!(step.finished && !step.matches, "{step:?}");
    assert!(step.halted && halted(&pic, canister));
}

#[test]
fn set_halted_refuses_a_stranger() {
    let (pic, canister, admin) = setup_with_rail_usdc();
    assert!(set_halted(&pic, canister, stranger(), true).is_err());
    assert!(set_halted(&pic, canister, quoter(), true).is_err());
    assert!(!halted(&pic, canister));
    set_halted(&pic, canister, admin, true).unwrap();
    assert!(halted(&pic, canister));
}

/// An upgrade is the first thing ops reach for, so the flag is stable: a halt
/// outlives the redeploy and only a deliberate call clears it.
#[test]
fn the_halt_flag_survives_an_upgrade() {
    let (pic, canister, admin) = setup_with_rail_usdc();
    set_halted(&pic, canister, admin, true).unwrap();

    upgrade(&pic, canister, admin).unwrap();

    assert!(halted(&pic, canister), "a redeploy is not an investigation");
    set_halted(&pic, canister, admin, false).unwrap();
    assert!(!halted(&pic, canister));
}
