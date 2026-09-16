use crate::client::settlement::{self, events_page, get_pending, register_quote, set_roles};
use crate::settlement_suite::init::setup;
use crate::wasms;
use candid::{encode_one, Principal};
use pocket_ic::{PocketIc, Time};
use settlement_api::types::events::{Event, EventEnvelope, Hash32};
use settlement_api::types::quote::{
    quote_hash, GasMode, Quote, MAX_QUOTE_LIFETIME_S, MAX_QUOTE_STRING_BYTES,
};

/// The same fixture as `types/quote/tests.rs`'s unit tests, field for field. The golden assert in
/// `the_quoter_registers_a_quote_and_gets_the_golden_hash` keeps the two identical.
fn fixed_quote() -> Quote {
    Quote {
        version: 1,
        src_chain: 8453,
        src_token: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".into(),
        amount_in: 25_000_000,
        dst_chain: 42161,
        dst_token: "0xaf88d065e77c8cC2239327C5EDb3A432268e5831".into(),
        expected_out: 24_990_000,
        min_out: 24_900_000,
        dst_address: "0x7551A66653f9a20979ed81835a0b7008EC83401b".into(),
        refund_address: None,
        auto_refund: true,
        gas_mode: GasMode::Legacy,
        rail: "cctp_v2_fast".into(),
        expires_at_s: 1_800_000_000,
        nonce: 7,
    }
}

const GOLDEN: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../canisters/settlement/api/golden/quote_hash_v1.txt"
);

fn golden_hash() -> Hash32 {
    let hex = std::fs::read_to_string(GOLDEN).expect("golden vector, committed");
    let bytes = hex::decode(hex.trim()).expect("golden vector is hex");
    bytes.try_into().expect("golden vector is 32 bytes")
}

fn quoter() -> Principal {
    Principal::from_slice(&[2; 29])
}

fn watcher() -> Principal {
    Principal::from_slice(&[3; 29])
}

fn stranger() -> Principal {
    Principal::from_slice(&[9; 29])
}

/// Seconds on the pic clock, which is what the canister compares an expiry against.
fn now_s(pic: &PocketIc) -> u64 {
    pic.get_time().as_nanos_since_unix_epoch() / 1_000_000_000
}

fn event_count(pic: &PocketIc, canister: Principal) -> u64 {
    settlement::event_count(pic, canister, stranger())
}

fn events(pic: &PocketIc, canister: Principal) -> Vec<EventEnvelope> {
    events_page(pic, canister, stranger(), 0, 100)
}

/// A canister with both roles handed out, its clock inside the fixture's validity window.
/// The fixture's `expires_at_s` is frozen by the cross-repo golden, so the clock moves to
/// the quote rather than the quote moving to the clock.
fn with_roles() -> (PocketIc, Principal, Principal) {
    let (pic, canister, admin) = setup();
    set_roles(&pic, canister, admin, quoter(), watcher()).unwrap();
    pic.set_time(Time::from_nanos_since_unix_epoch(
        (fixed_quote().expires_at_s - 60) * 1_000_000_000,
    ));
    (pic, canister, admin)
}

/// Unset roles must refuse rather than fall open, and they must refuse the controller
/// too: being able to *set* the quoter is not being the quoter.
#[test]
fn register_quote_refuses_everyone_while_the_roles_are_unset() {
    let (pic, canister, admin) = setup();
    for caller in [admin, quoter(), stranger()] {
        let err =
            register_quote(&pic, canister, caller, &fixed_quote()).expect_err("no role is set yet");
        assert!(err.contains("not set"), "say the role is unset: {err}");
        let err = get_pending(&pic, canister, caller, [0; 32]).expect_err("nor is the reader open");
        assert!(err.contains("not set"), "say the roles are unset: {err}");
    }
}

#[test]
fn set_roles_refuses_a_stranger_and_changes_nothing() {
    let (pic, canister, _) = setup();
    assert!(set_roles(&pic, canister, stranger(), stranger(), watcher()).is_err());
    assert!(register_quote(&pic, canister, stranger(), &fixed_quote()).is_err());
}

/// Rotation is the point of holding the roles in a writable cell: a second `set_roles`
/// must revoke the old quoter, not add a second one.
#[test]
fn set_roles_rotates_the_quoter_and_the_old_one_loses_access() {
    let (pic, canister, admin) = with_roles();
    let next = Principal::from_slice(&[4; 29]);
    set_roles(&pic, canister, admin, next, watcher()).unwrap();

    assert!(register_quote(&pic, canister, next, &fixed_quote()).is_ok());
    assert!(
        register_quote(&pic, canister, quoter(), &fixed_quote()).is_err(),
        "the replaced quoter is out"
    );
}

/// The anonymous principal is every unauthenticated caller at once, so handing it a role
/// would open the endpoint to the world.
#[test]
fn set_roles_refuses_the_anonymous_principal() {
    let (pic, canister, admin) = setup();
    let err = set_roles(&pic, canister, admin, Principal::anonymous(), watcher())
        .expect_err("anonymous is not a service identity");
    assert!(err.contains("anonymous"), "say why: {err}");
    let err = set_roles(&pic, canister, admin, quoter(), Principal::anonymous())
        .expect_err("either slot");
    assert!(err.contains("anonymous"), "say why: {err}");
}

/// The cross-repo contract, end to end: what the endpoint returns is the golden hash that
/// swapic-backend's quoter computes on its own side.
#[test]
fn the_quoter_registers_a_quote_and_gets_the_golden_hash() {
    let (pic, canister, _) = with_roles();
    let before = event_count(&pic, canister);
    let hash = register_quote(&pic, canister, quoter(), &fixed_quote()).unwrap();
    assert_eq!(hash, golden_hash());
    assert_eq!(
        get_pending(&pic, canister, quoter(), hash).unwrap(),
        Some(fixed_quote())
    );
    // pre-money: a registered quote is not something that happened to money
    assert_eq!(
        event_count(&pic, canister),
        before,
        "registering writes no event"
    );
}

/// A rotation changes who may move money, so it leaves an audit line naming both
/// principals in the world-readable log.
#[test]
fn set_roles_lands_a_roles_changed_event() {
    let (pic, canister, _) = with_roles();
    let logged = events(&pic, canister);
    assert_eq!(logged.len(), 1, "one rotation, one event");
    assert_eq!(
        logged[0].event,
        Event::RolesChanged {
            quoter: quoter().to_text(),
            watcher: watcher().to_text(),
        }
    );
}

/// A refused rotation must leave no audit line either: the event and the cell move
/// together or not at all.
#[test]
fn a_refused_set_roles_writes_no_event() {
    let (pic, canister, admin) = setup();
    assert!(set_roles(&pic, canister, stranger(), quoter(), watcher()).is_err());
    assert!(set_roles(&pic, canister, admin, Principal::anonymous(), watcher()).is_err());
    assert!(events(&pic, canister).is_empty());
}

#[test]
fn register_quote_refuses_a_stranger_and_the_watcher() {
    let (pic, canister, admin) = with_roles();
    for caller in [stranger(), watcher(), admin] {
        assert!(
            register_quote(&pic, canister, caller, &fixed_quote()).is_err(),
            "only the quoter registers quotes"
        );
    }
    assert_eq!(
        get_pending(&pic, canister, quoter(), golden_hash()).unwrap(),
        None,
        "and a refused call stores nothing"
    );
}

#[test]
fn register_quote_refuses_a_quote_that_has_already_expired() {
    let (pic, canister, _) = with_roles();
    let stale = Quote {
        expires_at_s: now_s(&pic) - 1,
        ..fixed_quote()
    };
    let err = register_quote(&pic, canister, quoter(), &stale).expect_err("the quote is stale");
    assert!(err.contains("expired"), "say why: {err}");

    // and the fixture is live on this clock, so the happy path above is not an accident
    assert!(now_s(&pic) < fixed_quote().expires_at_s);
}

/// The far end of the window, through the endpoint's own clock: a quote that would sit in
/// the store for a year cannot take a slot.
#[test]
fn register_quote_refuses_a_quote_that_expires_too_far_ahead() {
    let (pic, canister, _) = with_roles();
    let immortal = Quote {
        expires_at_s: now_s(&pic) + MAX_QUOTE_LIFETIME_S + 60,
        ..fixed_quote()
    };
    let err =
        register_quote(&pic, canister, quoter(), &immortal).expect_err("that is too far ahead");
    assert!(
        err.contains(&MAX_QUOTE_LIFETIME_S.to_string()),
        "say why: {err}"
    );
}

/// The byte cap through the endpoint: one byte over is refused naming the field and stores
/// nothing, and the cap itself registers.
#[test]
fn register_quote_refuses_a_string_over_the_byte_cap() {
    let (pic, canister, _) = with_roles();
    let over = Quote {
        dst_address: "a".repeat(MAX_QUOTE_STRING_BYTES + 1),
        ..fixed_quote()
    };
    let err = register_quote(&pic, canister, quoter(), &over).expect_err("one byte over the cap");
    assert!(err.contains("dst_address"), "name the field: {err}");
    assert_eq!(
        get_pending(&pic, canister, quoter(), quote_hash(&over)).unwrap(),
        None,
        "and nothing was stored"
    );

    let at_cap = Quote {
        dst_address: "a".repeat(MAX_QUOTE_STRING_BYTES),
        ..fixed_quote()
    };
    let hash = register_quote(&pic, canister, quoter(), &at_cap).expect("the cap itself registers");
    assert_eq!(
        get_pending(&pic, canister, quoter(), hash).unwrap(),
        Some(at_cap)
    );
}

/// A pending quote carries the user's destination and refund addresses, so it is not
/// public. Both services read it; nobody else does, and the refusal is the same whether
/// or not the hash names a real quote.
#[test]
fn get_pending_answers_both_services_and_refuses_a_stranger() {
    let (pic, canister, admin) = with_roles();
    let hash = register_quote(&pic, canister, quoter(), &fixed_quote()).unwrap();

    for caller in [quoter(), watcher()] {
        assert_eq!(
            get_pending(&pic, canister, caller, hash).unwrap(),
            Some(fixed_quote())
        );
        assert_eq!(
            get_pending(&pic, canister, caller, [0; 32]).unwrap(),
            None,
            "an unknown hash is an answer, not an error"
        );
    }

    for caller in [stranger(), admin] {
        let real = get_pending(&pic, canister, caller, hash).unwrap_err();
        let unknown = get_pending(&pic, canister, caller, [0; 32]).unwrap_err();
        assert_eq!(
            real, unknown,
            "the refusal must not say whether the hash exists"
        );
    }
}

/// The roles are in a stable cell and the pending store deliberately is not: the two
/// halves of that decision, pinned in one test.
#[test]
fn roles_survive_an_upgrade_and_the_pending_store_does_not() {
    let (pic, canister, admin) = with_roles();
    let hash = register_quote(&pic, canister, quoter(), &fixed_quote()).unwrap();

    pic.upgrade_canister(
        canister,
        wasms::settlement(),
        encode_one(()).unwrap(),
        Some(admin),
    )
    .unwrap();

    assert_eq!(
        get_pending(&pic, canister, quoter(), hash).unwrap(),
        None,
        "pre-money state is rebuilt empty; the quoter re-opens what is still live"
    );
    // still the quoter, without a second set_roles
    assert_eq!(
        register_quote(&pic, canister, quoter(), &fixed_quote()).unwrap(),
        hash
    );
    assert!(register_quote(&pic, canister, stranger(), &fixed_quote()).is_err());
}
