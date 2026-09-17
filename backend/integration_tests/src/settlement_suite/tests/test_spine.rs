use crate::client::pocket::query;
use crate::client::settlement::{append, event_count, events_page, get_swap, test_skew_state};
use crate::settlement_suite::init::setup;
use crate::wasms;
use candid::{encode_one, Nat, Principal};
use pocket_ic::PocketIc;
use settlement_api::types::errors::{AppendError, GuardError, TestAppendError};
use settlement_api::types::events::{Event, EventType};
use settlement_api::types::swap::TransitionError;

const QUOTE: [u8; 32] = [7; 32];

fn swap_sequence() -> Vec<EventType> {
    vec![
        EventType::PocketFunded {
            chain_id: 8453,
            amount: Nat::from(1_000_000_u64),
        },
        // the first event carrying blob fields through stable storage
        EventType::FundsReceived {
            quote_hash: QUOTE,
            quote_bytes: vec![0xde, 0xad, 0xbe, 0xef],
            chain_id: 8453,
            token: "USDC".into(),
            amount: Nat::from(1000_u64),
            tx_ref: "0xfeed".into(),
        },
        EventType::TxSigned {
            quote_hash: QUOTE,
            attempt: 1,
            chain_id: 8453,
            tx_hash: [1; 32],
            raw_tx: vec![0x02, 0xf8, 0x6b],
        },
        EventType::TxConfirmed {
            quote_hash: QUOTE,
            attempt: 1,
            chain_id: 8453,
            tx_hash: [1; 32],
            block: 19_000_000,
        },
        EventType::PaidInStable {
            quote_hash: QUOTE,
            chain_id: 42161,
            amount: Nat::from(999_u64),
        },
        EventType::SwapDone { quote_hash: QUOTE },
    ]
}

fn count(pic: &PocketIc, canister: Principal, who: Principal) -> u64 {
    event_count(pic, canister, who)
}

fn page(pic: &PocketIc, canister: Principal, who: Principal, start: u64, len: u64) -> Vec<Event> {
    events_page(pic, canister, who, start, len)
}

fn verify(pic: &PocketIc, canister: Principal, who: Principal, method: &str) -> bool {
    query(pic, canister, who, method, encode_one(()).unwrap())
}

/// The test-only door that flips one bit of the fold's chain head and leaves the log alone.
/// Calling it twice puts the head back.
fn skew(pic: &PocketIc, canister: Principal, who: Principal) -> Result<(), GuardError> {
    test_skew_state(pic, canister, who)
}

#[test]
fn spine_holds_across_a_whole_swap_and_an_upgrade() {
    let (pic, canister, admin) = setup();
    // the install's config and roles events come first
    let installed = count(&pic, canister, admin);

    let events = swap_sequence();
    let total = installed + events.len() as u64;
    for (i, event) in (installed..).zip(&events) {
        let index = append(&pic, canister, admin, event).expect("guard admits");
        assert_eq!(index, i, "events are numbered without gaps");
    }
    assert_eq!(count(&pic, canister, admin), total);

    // a rejected event must leave no trace in the log
    let dup = &swap_sequence()[1];
    let err = append(&pic, canister, admin, dup).expect_err("guard rejects");
    assert_eq!(
        err,
        TestAppendError::Append(AppendError::Transition(TransitionError::SwapExists(QUOTE))),
        "the swap already has funds"
    );
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
    for (i, env) in (0..).zip(&full) {
        assert_eq!(env.index, i);
    }
    let swap_events: Vec<EventType> = full[installed as usize..]
        .iter()
        .map(|env| env.payload.clone())
        .collect();
    assert_eq!(swap_events, events);
    assert!(page(&pic, canister, admin, total + 10, 10).is_empty());
    assert_eq!(page(&pic, canister, admin, 0, 10_000).len() as u64, total);

    let swap = get_swap(&pic, canister, admin, QUOTE).expect("the swap exists");

    pic.upgrade_canister(
        canister,
        wasms::settlement(),
        encode_one(()).unwrap(),
        Some(admin),
    )
    .unwrap();

    assert_eq!(
        get_swap(&pic, canister, admin, QUOTE),
        Some(swap),
        "the fold is stable memory, so it comes through the upgrade without a replay"
    );
    assert_eq!(count(&pic, canister, admin), total);
    assert!(verify(&pic, canister, admin, "verify_chain"));
    assert!(verify(&pic, canister, admin, "verify_replay"));
    assert_eq!(
        page(&pic, canister, admin, 0, total),
        full,
        "the log reads back byte for byte after the upgrade"
    );
}

/// The divergence the index check cannot see: the fold's chain head moved while the log
/// stayed put, so the next event would seal on a parent hash the log does not end with and
/// fork the chain at an index that looks perfectly right. Covered on the log the install
/// wrote and on one that also holds a whole swap. An install always writes its config and
/// roles events, so the genesis arm, where the head is the zero hash, is covered by the
/// storage unit test `the_fold_check_passes_a_fold_in_step_and_names_what_is_not`.
#[test]
fn append_refuses_to_seal_on_a_chain_head_the_log_does_not_end_with() {
    let (pic, canister, admin) = setup();
    let first = &swap_sequence()[0];
    let installed = count(&pic, canister, admin);

    // the install's log: the fold's head is flipped, and nothing is admissible
    skew(&pic, canister, admin).unwrap();
    let err = append(&pic, canister, admin, first).expect_err("the head diverged");
    assert!(
        matches!(
            err,
            TestAppendError::Append(AppendError::ChainDiverged { .. })
        ),
        "say what went wrong: {err:?}"
    );
    assert_eq!(
        count(&pic, canister, admin),
        installed,
        "and nothing was written"
    );

    // the same flip puts the head back, so what refused above was the head check and not
    // the transition guard
    skew(&pic, canister, admin).unwrap();
    let events = swap_sequence();
    for (i, event) in (installed..).zip(&events) {
        assert_eq!(
            append(&pic, canister, admin, event).expect("the head lines up again"),
            i
        );
    }
    let total = installed + events.len() as u64;

    // and the same divergence over a log that already holds a whole swap
    skew(&pic, canister, admin).unwrap();
    let more = EventType::PocketFunded {
        chain_id: 8453,
        amount: Nat::from(1_u64),
    };
    let err = append(&pic, canister, admin, &more).expect_err("the head diverged");
    assert!(
        matches!(
            err,
            TestAppendError::Append(AppendError::ChainDiverged { .. })
        ),
        "say what went wrong: {err:?}"
    );
    assert_eq!(count(&pic, canister, admin), total, "the log is untouched");

    // put the head back and the canister is whole again: the skew only ever moved the fold
    skew(&pic, canister, admin).unwrap();
    assert!(verify(&pic, canister, admin, "verify_chain"));
    assert!(verify(&pic, canister, admin, "verify_replay"));
    assert!(append(&pic, canister, admin, &more).is_ok());
}

/// The skew door is as controller-only as `test_append`: a stranger cannot corrupt the fold.
#[test]
fn test_skew_state_rejects_non_controller() {
    let (pic, canister, admin) = setup();
    let stranger = Principal::from_slice(&[9; 29]);
    assert!(skew(&pic, canister, stranger).is_err());
    assert!(append(&pic, canister, admin, &swap_sequence()[0]).is_ok());
}
