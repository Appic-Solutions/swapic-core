use crate::client::pocket::query;
use crate::client::settlement::{append, event_count, events_page};
use crate::settlement_suite::init::{setup, upgrade};
use candid::{encode_one, Nat, Principal};
use pocket_ic::PocketIc;
use settlement_api::types::errors::{AppendError, TestAppendError};
use settlement_api::types::events::{Event, EventType, Hash32, TxPurpose};
use settlement_api::types::transition::TransitionError;

/// The key the spine's relay is signed under.
fn relay_key() -> Hash32 {
    [0x7e; 32]
}

/// A flow step as the wire carries it.
fn flow(step: types::FlowStep) -> EventType {
    EventType::from(types::EventType::Flow {
        quote_hash: types::QuoteHash::new(relay_key()),
        step,
    })
}

/// A whole relay: its transaction handed out, its signed bytes recorded (the first lines
/// carrying blob fields through stable storage), its receipt at depth, and the fee it paid
/// booked.
fn relay_sequence() -> Vec<EventType> {
    vec![
        EventType::TxCreated {
            purpose: TxPurpose::Relay(relay_key()),
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
        flow(types::FlowStep::GaslessRelayed {
            chain_id: types::ChainId::BASE,
            nonce: types::Nonce::ZERO,
            tx_hash: types::TxHash::new([1; 32]),
            raw_tx: vec![0x02, 0xf8, 0x6b],
        }),
        flow(types::FlowStep::TxClosed {
            tx_hash: types::TxHash::new([1; 32]),
            block: types::BlockNumber::new(19_000_000),
            success: true,
        }),
        EventType::from(types::EventType::Books {
            chain_id: types::ChainId::BASE,
            step: types::books::BookStep::FeeEarned {
                tx_hash: types::TxHash::new([1; 32]),
                amount: types::TokenAmount::from(1_000_u32),
                block: Some(types::BlockNumber::new(19_000_000)),
            },
        }),
    ]
}

/// Whether `err` is the refusal of a relay whose flow exists: one relay is signed once.
fn refused_as_relayed(err: &TestAppendError) -> bool {
    matches!(
        err,
        TestAppendError::Append(AppendError::Transition(TransitionError::Flow(_)))
    )
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

#[test]
fn spine_holds_across_a_whole_relay_and_an_upgrade() {
    let (pic, canister, admin) = setup();
    // the install's config and roles events come first
    let installed = count(&pic, canister, admin);

    let events = relay_sequence();
    let total = installed + events.len() as u64;
    for (i, event) in (installed..).zip(&events) {
        let index = append(&pic, canister, admin, event).expect("guard admits");
        assert_eq!(index, i, "events are numbered without gaps");
    }
    assert_eq!(count(&pic, canister, admin), total);

    // a rejected event must leave no trace in the log
    let dup = &relay_sequence()[0];
    let err = append(&pic, canister, admin, dup).expect_err("guard rejects");
    assert!(
        refused_as_relayed(&err),
        "the relay was signed already: {err:?}"
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
    let relay_events: Vec<EventType> = full[installed as usize..]
        .iter()
        .map(|env| env.payload.clone())
        .collect();
    assert_eq!(relay_events, events);
    assert!(page(&pic, canister, admin, total + 10, 10).is_empty());
    assert_eq!(page(&pic, canister, admin, 0, 10_000).len() as u64, total);

    upgrade(&pic, canister, admin).unwrap();

    // the fold is stable memory, so it comes through the upgrade without a replay: the
    // relay's flow is still there to refuse its creation a second time
    let err = append(&pic, canister, admin, dup).expect_err("guard rejects");
    assert!(refused_as_relayed(&err), "{err:?}");
    assert_eq!(count(&pic, canister, admin), total);
    assert!(verify(&pic, canister, admin, "verify_chain"));
    assert!(verify(&pic, canister, admin, "verify_replay"));
    assert_eq!(
        page(&pic, canister, admin, 0, total),
        full,
        "the log reads back byte for byte after the upgrade"
    );
}
