use crate::settlement_suite::init::{setup, upgrade};
use candid::{decode_one, encode_one, Principal};
use settlement_api::types::errors::TestAppendError;
use settlement_api::types::events::EventType;

/// A fee the books earned on Base, `units` of USDC: a line the fold admits and folds.
fn fee(units: u64) -> EventType {
    EventType::from(types::EventType::Books {
        chain_id: types::ChainId::BASE,
        step: types::books::BookStep::FeeEarned {
            tx_hash: types::TxHash::new([units as u8; 32]),
            amount: types::TokenAmount::from(units),
            block: None,
        },
    })
}

#[test]
fn events_survive_upgrade() {
    let (pic, canister, admin) = setup();
    let raw = pic
        .query_call(canister, admin, "event_count", encode_one(()).unwrap())
        .unwrap();
    let installed = decode_one::<u64>(&raw).unwrap();
    let event = fee(1000);
    let raw = pic
        .update_call(canister, admin, "test_append", encode_one(&event).unwrap())
        .unwrap();
    decode_one::<Result<u64, TestAppendError>>(&raw)
        .unwrap()
        .unwrap();

    upgrade(&pic, canister, admin).unwrap();

    let raw = pic
        .query_call(canister, admin, "event_count", encode_one(()).unwrap())
        .unwrap();
    assert_eq!(decode_one::<u64>(&raw).unwrap(), installed + 1);
}

#[test]
fn test_append_rejects_non_controller() {
    let (pic, canister, _) = setup();
    let stranger = Principal::from_slice(&[9; 29]);
    let raw = pic
        .query_call(canister, stranger, "event_count", encode_one(()).unwrap())
        .unwrap();
    let installed = decode_one::<u64>(&raw).unwrap();
    let event = fee(1);
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
