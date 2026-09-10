mod common;

use candid::{decode_one, encode_one, Principal};

#[test]
fn events_survive_upgrade_and_replay_matches() {
    let (pic, canister, admin) = common::setup();
    let event = settlement::events::Event::PocketFunded {
        chain_id: 8453,
        amount: 1000,
    };
    let raw = pic
        .update_call(canister, admin, "test_append", encode_one(&event).unwrap())
        .unwrap();
    decode_one::<Result<u64, String>>(&raw).unwrap().unwrap();

    // before the upgrade the live state was built incrementally by append_event, so this
    // compares that against a fresh fold; after the upgrade it would compare a fold to itself
    let raw = pic
        .query_call(canister, admin, "verify_replay", encode_one(()).unwrap())
        .unwrap();
    assert!(
        decode_one::<bool>(&raw).unwrap(),
        "incremental apply matches a fresh fold"
    );

    pic.upgrade_canister(
        canister,
        common::wasm(),
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
    let (pic, canister, _) = common::setup();
    let stranger = Principal::from_slice(&[9; 29]);
    let event = settlement::events::Event::PocketFunded {
        chain_id: 8453,
        amount: 1,
    };
    let raw = pic
        .update_call(
            canister,
            stranger,
            "test_append",
            encode_one(&event).unwrap(),
        )
        .unwrap();
    assert!(decode_one::<Result<u64, String>>(&raw).unwrap().is_err());

    // a rejected caller writes nothing
    let raw = pic
        .query_call(canister, stranger, "event_count", encode_one(()).unwrap())
        .unwrap();
    assert_eq!(decode_one::<u64>(&raw).unwrap(), 0);
}
