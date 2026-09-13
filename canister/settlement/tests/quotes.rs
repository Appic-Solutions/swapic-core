mod common;

use candid::{decode_one, encode_args, encode_one, Principal};
use pocket_ic::PocketIc;
use settlement::events::Hash32;
use settlement::quote::{GasMode, Quote};

/// The same fixture as `quote.rs`'s unit tests, field for field. The golden assert in
/// `the_quoter_opens_a_quote_and_gets_the_golden_hash` is what keeps the two identical.
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
    "/tests/golden/quote_hash_v1.txt"
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

fn set_roles(
    pic: &PocketIc,
    canister: Principal,
    sender: Principal,
    q: Principal,
    w: Principal,
) -> Result<(), String> {
    let raw = pic
        .update_call(canister, sender, "set_roles", encode_args((q, w)).unwrap())
        .unwrap();
    decode_one(&raw).unwrap()
}

fn open_gasless(
    pic: &PocketIc,
    canister: Principal,
    sender: Principal,
    quote: &Quote,
) -> Result<Hash32, String> {
    let raw = pic
        .update_call(canister, sender, "open_gasless", encode_one(quote).unwrap())
        .unwrap();
    decode_one(&raw).unwrap()
}

fn get_pending(
    pic: &PocketIc,
    canister: Principal,
    sender: Principal,
    hash: Hash32,
) -> Result<Option<Quote>, String> {
    common::query(
        pic,
        canister,
        sender,
        "get_pending",
        encode_one(hash).unwrap(),
    )
}

fn event_count(pic: &PocketIc, canister: Principal) -> u64 {
    common::query(
        pic,
        canister,
        stranger(),
        "event_count",
        encode_one(()).unwrap(),
    )
}

/// A canister with both roles handed out.
fn with_roles() -> (PocketIc, Principal, Principal) {
    let (pic, canister, admin) = common::setup();
    set_roles(&pic, canister, admin, quoter(), watcher()).unwrap();
    (pic, canister, admin)
}

/// Unset roles must refuse rather than fall open, and they must refuse the controller
/// too: being able to *set* the quoter is not being the quoter.
#[test]
fn open_gasless_refuses_everyone_while_the_roles_are_unset() {
    let (pic, canister, admin) = common::setup();
    for caller in [admin, quoter(), stranger()] {
        let err =
            open_gasless(&pic, canister, caller, &fixed_quote()).expect_err("no role is set yet");
        assert!(err.contains("not set"), "say the role is unset: {err}");
    }
}

#[test]
fn set_roles_refuses_a_stranger_and_changes_nothing() {
    let (pic, canister, _) = common::setup();
    assert!(set_roles(&pic, canister, stranger(), stranger(), watcher()).is_err());
    assert!(open_gasless(&pic, canister, stranger(), &fixed_quote()).is_err());
}

/// Rotation is the point of holding the roles in a writable cell: a second `set_roles`
/// must revoke the old quoter, not add a second one.
#[test]
fn set_roles_rotates_the_quoter_and_the_old_one_loses_access() {
    let (pic, canister, admin) = with_roles();
    let next = Principal::from_slice(&[4; 29]);
    set_roles(&pic, canister, admin, next, watcher()).unwrap();

    assert!(open_gasless(&pic, canister, next, &fixed_quote()).is_ok());
    assert!(
        open_gasless(&pic, canister, quoter(), &fixed_quote()).is_err(),
        "the replaced quoter is out"
    );
}

/// The anonymous principal is every unauthenticated caller at once, so handing it a role
/// would open the endpoint to the world.
#[test]
fn set_roles_refuses_the_anonymous_principal() {
    let (pic, canister, admin) = common::setup();
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
fn the_quoter_opens_a_quote_and_gets_the_golden_hash() {
    let (pic, canister, _) = with_roles();
    let hash = open_gasless(&pic, canister, quoter(), &fixed_quote()).unwrap();
    assert_eq!(hash, golden_hash());
    assert_eq!(
        get_pending(&pic, canister, quoter(), hash).unwrap(),
        Some(fixed_quote())
    );
    // pre-money: an opened quote is not something that happened to money
    assert_eq!(event_count(&pic, canister), 0, "the log stays empty");
}

#[test]
fn open_gasless_refuses_a_stranger_and_the_watcher() {
    let (pic, canister, admin) = with_roles();
    for caller in [stranger(), watcher(), admin] {
        assert!(
            open_gasless(&pic, canister, caller, &fixed_quote()).is_err(),
            "only the quoter opens quotes"
        );
    }
    assert_eq!(
        get_pending(&pic, canister, quoter(), golden_hash()).unwrap(),
        None,
        "and a refused call stores nothing"
    );
}

#[test]
fn open_gasless_refuses_a_quote_that_has_already_expired() {
    let (pic, canister, _) = with_roles();
    let stale = Quote {
        expires_at_s: now_s(&pic) - 1,
        ..fixed_quote()
    };
    let err = open_gasless(&pic, canister, quoter(), &stale).expect_err("the quote is stale");
    assert!(err.contains("expired"), "say why: {err}");

    // and the fixture is live on this clock, so the happy path above is not an accident
    assert!(now_s(&pic) < fixed_quote().expires_at_s);
}

/// A pending quote carries the user's destination and refund addresses, so it is not
/// public. Both services read it; nobody else does, and the refusal is the same whether
/// or not the hash names a real quote.
#[test]
fn get_pending_answers_both_services_and_refuses_a_stranger() {
    let (pic, canister, admin) = with_roles();
    let hash = open_gasless(&pic, canister, quoter(), &fixed_quote()).unwrap();

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
    let hash = open_gasless(&pic, canister, quoter(), &fixed_quote()).unwrap();

    pic.upgrade_canister(
        canister,
        common::wasm(),
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
        open_gasless(&pic, canister, quoter(), &fixed_quote()).unwrap(),
        hash
    );
    assert!(open_gasless(&pic, canister, stranger(), &fixed_quote()).is_err());
}
