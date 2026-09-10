mod common;

use candid::{encode_args, encode_one, Principal};
use pocket_ic::PocketIc;
use settlement::events::{Event, EventEnvelope};

const QUOTE: [u8; 32] = [7; 32];

fn swap_sequence() -> Vec<Event> {
    vec![
        Event::PocketFunded {
            chain_id: 8453,
            amount: 1_000_000,
        },
        // the first event carrying blob fields through stable storage
        Event::FundsReceived {
            quote_hash: QUOTE,
            quote_bytes: vec![0xde, 0xad, 0xbe, 0xef],
            chain_id: 8453,
            token: "USDC".into(),
            amount: 1000,
            tx_ref: "0xfeed".into(),
        },
        Event::TxSigned {
            quote_hash: QUOTE,
            attempt: 1,
            chain_id: 8453,
            tx_hash: [1; 32],
            raw_tx: vec![0x02, 0xf8, 0x6b],
        },
        Event::TxConfirmed {
            quote_hash: QUOTE,
            attempt: 1,
            chain_id: 8453,
            tx_hash: [1; 32],
            block: 19_000_000,
        },
        Event::PaidInStable {
            quote_hash: QUOTE,
            chain_id: 42161,
            amount: 999,
        },
        Event::SwapDone { quote_hash: QUOTE },
    ]
}

fn count(pic: &PocketIc, canister: Principal, who: Principal) -> u64 {
    common::query(pic, canister, who, "event_count", encode_one(()).unwrap())
}

fn page(
    pic: &PocketIc,
    canister: Principal,
    who: Principal,
    start: u64,
    len: u64,
) -> Vec<EventEnvelope> {
    common::query(
        pic,
        canister,
        who,
        "events_page",
        encode_args((start, len)).unwrap(),
    )
}

fn verify(pic: &PocketIc, canister: Principal, who: Principal, method: &str) -> bool {
    common::query(pic, canister, who, method, encode_one(()).unwrap())
}

#[test]
fn spine_holds_across_a_whole_swap_and_an_upgrade() {
    let (pic, canister, admin) = common::setup();

    let events = swap_sequence();
    let total = events.len() as u64;
    for (i, event) in events.iter().enumerate() {
        let index = common::append(&pic, canister, admin, event).expect("guard admits");
        assert_eq!(index, i as u64, "events are numbered without gaps");
    }
    assert_eq!(count(&pic, canister, admin), total);

    // a rejected event must leave no trace in the log
    let dup = &swap_sequence()[1];
    let err = common::append(&pic, canister, admin, dup).expect_err("guard rejects");
    assert!(err.contains("already has funds"), "unexpected error: {err}");
    assert_eq!(
        count(&pic, canister, admin),
        total,
        "rejection wrote nothing"
    );

    assert!(verify(&pic, canister, admin, "verify_chain"));
    assert!(verify(&pic, canister, admin, "verify_replay"));

    // paging boundaries: the exact range, a start past the end, and more than exists
    let full = page(&pic, canister, admin, 0, total);
    assert_eq!(full.len() as u64, total);
    for (i, env) in full.iter().enumerate() {
        assert_eq!(env.index, i as u64);
        assert_eq!(env.event, events[i]);
    }
    assert!(page(&pic, canister, admin, total + 10, 10).is_empty());
    assert_eq!(page(&pic, canister, admin, 0, 10_000).len() as u64, total);

    pic.upgrade_canister(
        canister,
        common::wasm(),
        encode_one(()).unwrap(),
        Some(admin),
    )
    .unwrap();

    assert_eq!(count(&pic, canister, admin), total);
    assert!(verify(&pic, canister, admin, "verify_chain"));
    assert!(verify(&pic, canister, admin, "verify_replay"));
    assert_eq!(
        page(&pic, canister, admin, 0, total),
        full,
        "the log reads back byte for byte after the upgrade"
    );
}
