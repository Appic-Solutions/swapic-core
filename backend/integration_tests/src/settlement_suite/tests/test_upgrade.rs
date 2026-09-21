use crate::client::settlement::{append, audit_replay_step, event_count, test_skew_state};
use crate::settlement_suite::init::setup;
use crate::wasms;
use candid::{decode_one, encode_one, Nat, Principal};
use settlement_api::types::errors::TestAppendError;
use settlement_api::types::events::EventType;

#[test]
fn events_survive_upgrade_and_replay_matches() {
    let (pic, canister, admin) = setup();
    let raw = pic
        .query_call(canister, admin, "event_count", encode_one(()).unwrap())
        .unwrap();
    let installed = decode_one::<u64>(&raw).unwrap();
    let event = EventType::PocketFunded {
        chain_id: 8453,
        amount: Nat::from(1000_u64),
    };
    let raw = pic
        .update_call(canister, admin, "test_append", encode_one(&event).unwrap())
        .unwrap();
    decode_one::<Result<u64, TestAppendError>>(&raw)
        .unwrap()
        .unwrap();

    // the stable fold was built incrementally by append_event, and the audit compares it
    // against a fresh fold of the log, both before the upgrade and after it
    let raw = pic
        .query_call(canister, admin, "verify_replay", encode_one(()).unwrap())
        .unwrap();
    assert!(
        decode_one::<bool>(&raw).unwrap(),
        "incremental apply matches a fresh fold"
    );

    pic.upgrade_canister(
        canister,
        wasms::settlement(),
        encode_one(()).unwrap(),
        Some(admin),
    )
    .unwrap();

    let raw = pic
        .query_call(canister, admin, "event_count", encode_one(()).unwrap())
        .unwrap();
    assert_eq!(decode_one::<u64>(&raw).unwrap(), installed + 1);
    let raw = pic
        .query_call(canister, admin, "verify_replay", encode_one(()).unwrap())
        .unwrap();
    assert!(decode_one::<bool>(&raw).unwrap());
    let raw = pic
        .query_call(canister, admin, "verify_chain", encode_one(()).unwrap())
        .unwrap();
    assert!(decode_one::<bool>(&raw).unwrap());
}

#[test]
fn test_append_rejects_non_controller() {
    let (pic, canister, _) = setup();
    let stranger = Principal::from_slice(&[9; 29]);
    let raw = pic
        .query_call(canister, stranger, "event_count", encode_one(()).unwrap())
        .unwrap();
    let installed = decode_one::<u64>(&raw).unwrap();
    let event = EventType::PocketFunded {
        chain_id: 8453,
        amount: Nat::from(1_u64),
    };
    let raw = pic
        .update_call(
            canister,
            stranger,
            "test_append",
            encode_one(&event).unwrap(),
        )
        .unwrap();
    assert!(decode_one::<Result<u64, TestAppendError>>(&raw)
        .unwrap()
        .is_err());

    // a rejected caller writes nothing
    let raw = pic
        .query_call(canister, stranger, "event_count", encode_one(()).unwrap())
        .unwrap();
    assert_eq!(decode_one::<u64>(&raw).unwrap(), installed);
}

/// An upgrade over a fold out of step with its log would take and then refuse every
/// append, so `post_upgrade` refuses it: the upgrade fails and the old wasm keeps running on
/// untouched memory. Putting the fold back in step lets the same upgrade through.
#[test]
fn an_upgrade_over_a_fold_out_of_step_with_its_log_is_rejected() {
    let (pic, canister, admin) = setup();
    let raw = pic
        .query_call(canister, admin, "event_count", encode_one(()).unwrap())
        .unwrap();
    let installed = decode_one::<u64>(&raw).unwrap();
    let event = EventType::PocketFunded {
        chain_id: 8453,
        amount: Nat::from(1000_u64),
    };
    let raw = pic
        .update_call(canister, admin, "test_append", encode_one(&event).unwrap())
        .unwrap();
    decode_one::<Result<u64, TestAppendError>>(&raw)
        .unwrap()
        .unwrap();
    test_skew_state(&pic, canister, admin).unwrap();

    let err = pic
        .upgrade_canister(
            canister,
            wasms::settlement(),
            encode_one(()).unwrap(),
            Some(admin),
        )
        .expect_err("the fold links to a head the log does not end with");
    assert!(
        err.reject_message.contains("out of step"),
        "say why: {}",
        err.reject_message
    );

    // still the old wasm and the old memory: the event is there and the skew is too
    let raw = pic
        .query_call(canister, admin, "event_count", encode_one(()).unwrap())
        .unwrap();
    assert_eq!(decode_one::<u64>(&raw).unwrap(), installed + 1);
    test_skew_state(&pic, canister, admin).unwrap();

    pic.upgrade_canister(
        canister,
        wasms::settlement(),
        encode_one(()).unwrap(),
        Some(admin),
    )
    .expect("a fold in step upgrades");
    let raw = pic
        .query_call(canister, admin, "verify_replay", encode_one(()).unwrap())
        .unwrap();
    assert!(decode_one::<bool>(&raw).unwrap());
}

/// The deep audit saves its fold between steps in stable memory, so an upgrade in the
/// middle of an audit loses nothing: the step after it carries on where the one before it
/// stopped.
#[test]
fn the_replay_cursor_survives_an_upgrade() {
    let (pic, canister, admin) = setup();
    let installed = event_count(&pic, canister, admin);
    for i in 1..=4_u64 {
        let event = EventType::PocketFunded {
            chain_id: 8453,
            amount: Nat::from(i),
        };
        append(&pic, canister, admin, &event).expect("the guard admits it");
    }
    let total = installed + 4;

    let before = audit_replay_step(&pic, canister, admin, 2).expect("a controller may");
    assert_eq!((before.folded_so_far, before.finished), (2, false));

    pic.upgrade_canister(
        canister,
        wasms::settlement(),
        encode_one(()).unwrap(),
        Some(admin),
    )
    .unwrap();

    let after = audit_replay_step(&pic, canister, admin, 2).expect("a controller may");
    assert_eq!(
        (after.folded_so_far, after.remaining, after.finished),
        (4, total - 4, false),
        "the fold carried on where it stopped"
    );
    let last = audit_replay_step(&pic, canister, admin, total).expect("a controller may");
    assert!(last.finished && last.matches && !last.halted, "{last:?}");
}
