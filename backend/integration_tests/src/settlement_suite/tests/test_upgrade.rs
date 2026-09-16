use crate::settlement_suite::init::setup;
use crate::wasms;
use candid::{decode_one, encode_one, Nat, Principal};
use settlement_api::types::errors::TestAppendError;
use settlement_api::types::events::EventType;

#[test]
fn events_survive_upgrade_and_replay_matches() {
    let (pic, canister, admin) = setup();
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
    assert_eq!(decode_one::<u64>(&raw).unwrap(), 1);
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
    assert_eq!(decode_one::<u64>(&raw).unwrap(), 0);
}
