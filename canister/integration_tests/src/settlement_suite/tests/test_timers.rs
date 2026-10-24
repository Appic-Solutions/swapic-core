use crate::client::settlement::{self, append, set_config, set_halted, test_skew_state};
use crate::settlement_suite::init::{quoter, rail_config, setup_with_rail_usdc, upgrade};
use candid::{Nat, Principal};
use pocket_ic::PocketIc;
use settlement_api::types::config::Config;
use settlement_api::types::errors::GuardError;
use settlement_api::types::events::{EventType, TxPurpose};
use std::time::Duration;

fn stranger() -> Principal {
    Principal::from_slice(&[9; 29])
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

/// A relay signed on Base, its transaction handed out and its signed bytes recorded: some
/// of the log for the audits to read.
fn relayed(pic: &PocketIc, canister: Principal, admin: Principal) {
    let key = types::QuoteHash::new([0x7e; 32]);
    append(
        pic,
        canister,
        admin,
        &EventType::TxCreated {
            purpose: TxPurpose::Relay(key.into_bytes()),
            chain_id: 8453,
            nonce: 0,
            to: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".to_string(),
            value_wei: Nat::from(0_u8),
            data: vec![0xde, 0xad],
            gas_limit: Nat::from(120_000_u64),
            max_fee_wei_per_gas: Nat::from(1_u8),
            max_priority_fee_wei_per_gas: Nat::from(1_u8),
            action: None,
        },
    )
    .expect("the allocation first");
    append(
        pic,
        canister,
        admin,
        &EventType::from(types::EventType::Flow {
            quote_hash: key,
            step: types::FlowStep::GaslessRelayed {
                chain_id: types::ChainId::BASE,
                nonce: types::Nonce::ZERO,
                tx_hash: types::TxHash::new([1; 32]),
                raw_tx: vec![0x02, 0xf8, 0x6b],
            },
        }),
    )
    .expect("then its signed record");
}

/// The audit timer is wired, and a healthy canister survives it: a false halt would freeze
/// every money path, so this is the more dangerous direction.
#[test]
fn the_replay_audit_runs_on_its_own_and_halts_nothing_healthy() {
    let (pic, canister, admin) = setup_with_rail_usdc();
    relayed(&pic, canister, admin);

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
    relayed(&pic, canister, admin);
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
