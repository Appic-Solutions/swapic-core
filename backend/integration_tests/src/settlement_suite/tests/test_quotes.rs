use crate::client::settlement::{
    self, clear_pending_quotes, events_page, get_pending, register_quote, set_roles,
};
use crate::settlement_suite::init::{quoter, setup, upgrade, watcher};
use candid::{Nat, Principal};
use pocket_ic::{PocketIc, Time};
use settlement_api::types::errors::{GuardError, RegisterQuoteError, Role, SetRolesError};
use settlement_api::types::events::{Event, EventType, Hash32};
use settlement_api::types::quote::{GasMode, Quote, QuoteError};
use sha2::{Digest, Sha256};
use types::address::MAX_TEXT_BYTES;
use types::quote::MAX_QUOTE_LIFETIME;

/// The same fixture as the types crate's `quote/tests.rs`, field for field. The golden assert in
/// `the_quoter_registers_a_quote_and_gets_the_golden_hash` keeps the two identical.
/// The cross-repo vector: the quote `golden/quote_hash_v1.txt` pins the hash of, field for
/// field. It names no refund address, which is a shape the preimage has to hold and the
/// pending store no longer takes (a swap nobody could be refunded on), so what registers
/// is [`fixed_quote`] and what the golden is checked against is this.
fn golden_quote() -> Quote {
    Quote {
        version: 1,
        src_chain: 8453,
        src_token: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".into(),
        amount_in: Nat::from(25_000_000_u32),
        dst_chain: 42161,
        dst_token: "0xaf88d065e77c8cC2239327C5EDb3A432268e5831".into(),
        expected_out: Nat::from(24_990_000_u32),
        min_out: Nat::from(24_900_000_u32),
        dst_address: "0x7551A66653f9a20979ed81835a0b7008EC83401b".into(),
        refund_address: None,
        auto_refund: true,
        gas_mode: GasMode::Legacy,
        rail: "cctp_v2_fast".into(),
        expires_at_s: 1_800_000_000,
        nonce: 7,
    }
}

/// The vector's quote with a refund address, which is what the store takes: a quote a
/// refund could never be paid on is one whose swap could only freeze with the user's
/// funds in the vault.
fn fixed_quote() -> Quote {
    Quote {
        refund_address: Some("0x1111111111111111111111111111111111111111".into()),
        ..golden_quote()
    }
}

const GOLDEN: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../libraries/types/golden/quote_hash_v1.txt"
);

const MAX_QUOTE_LIFETIME_S: u64 = MAX_QUOTE_LIFETIME.as_secs();

fn golden_hash() -> Hash32 {
    let hex = std::fs::read_to_string(GOLDEN).expect("golden vector, committed");
    let bytes = hex::decode(hex.trim()).expect("golden vector is hex");
    bytes.try_into().expect("golden vector is 32 bytes")
}

/// The swap id a wire quote would have: sha256 over its canonical preimage, written here
/// byte by byte from the layout rather than through the types crate, so it exists even for
/// a quote the canister refuses to convert.
fn hash_by_hand(q: &Quote) -> Hash32 {
    let mut preimage = Vec::new();
    let text = |preimage: &mut Vec<u8>, s: &str| {
        preimage.extend_from_slice(&u32::try_from(s.len()).unwrap().to_be_bytes());
        preimage.extend_from_slice(s.as_bytes());
    };
    let amount = |n: &Nat| u128::try_from(&n.0).expect("a u128 amount").to_be_bytes();
    preimage.push(q.version);
    preimage.extend_from_slice(&q.src_chain.to_be_bytes());
    text(&mut preimage, &q.src_token);
    preimage.extend_from_slice(&amount(&q.amount_in));
    preimage.extend_from_slice(&q.dst_chain.to_be_bytes());
    text(&mut preimage, &q.dst_token);
    preimage.extend_from_slice(&amount(&q.expected_out));
    preimage.extend_from_slice(&amount(&q.min_out));
    text(&mut preimage, &q.dst_address);
    text(&mut preimage, q.refund_address.as_deref().unwrap_or(""));
    preimage.push(u8::from(q.auto_refund));
    preimage.push(match q.gas_mode {
        GasMode::Gasless => 0,
        GasMode::Legacy => 1,
    });
    text(&mut preimage, &q.rail);
    preimage.extend_from_slice(&q.expires_at_s.to_be_bytes());
    preimage.extend_from_slice(&q.nonce.to_be_bytes());
    Sha256::digest(preimage).into()
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

fn events(pic: &PocketIc, canister: Principal) -> Vec<Event> {
    events_page(pic, canister, stranger(), 0, 100)
}

/// A canister with both roles handed out at install, its clock inside the fixture's
/// validity window. The fixture's `expires_at_s` is frozen by the cross-repo golden, so the
/// clock moves to the quote rather than the quote moving to the clock.
fn with_roles() -> (PocketIc, Principal, Principal) {
    let (pic, canister, admin) = setup();
    pic.set_time(Time::from_nanos_since_unix_epoch(
        (fixed_quote().expires_at_s - 60) * 1_000_000_000,
    ));
    (pic, canister, admin)
}

/// The install names both roles, so a fresh canister is never without them, and they
/// refuse the controller too: being able to *set* the quoter is not being the quoter.
#[test]
fn register_quote_refuses_everyone_but_the_quoter_the_install_named() {
    let (pic, canister, admin) = with_roles();
    for caller in [admin, stranger()] {
        let err = register_quote(&pic, canister, caller, &fixed_quote())
            .expect_err("only the installed quoter registers");
        assert_eq!(
            err,
            RegisterQuoteError::Guard(GuardError::CallerNotRole(Role::Quoter)),
            "say the caller is not the quoter"
        );
        let err = get_pending(&pic, canister, caller, [0; 32]).expect_err("nor is the reader open");
        assert_eq!(err, GuardError::CallerNotQuoterOrWatcher);
    }
    assert!(register_quote(&pic, canister, quoter(), &fixed_quote()).is_ok());
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
    assert_eq!(err, SetRolesError::AnonymousRole(Role::Quoter), "say why");
    let err = set_roles(&pic, canister, admin, quoter(), Principal::anonymous())
        .expect_err("either slot");
    assert_eq!(err, SetRolesError::AnonymousRole(Role::Watcher), "say why");
}

/// The cross-repo contract, end to end: what the endpoint returns is the id the other
/// side computes on its own, written here byte by byte from the frozen layout, and that
/// writer is pinned against the golden vector in the same breath. The vector's own quote
/// names no refund address, which the store refuses, so the vector is checked through the
/// writer and the endpoint through the quote a quoter would really register.
#[test]
fn the_quoter_registers_a_quote_and_gets_the_golden_hash() {
    let (pic, canister, _) = with_roles();
    let before = event_count(&pic, canister);
    assert_eq!(hash_by_hand(&golden_quote()), golden_hash());
    let hash = register_quote(&pic, canister, quoter(), &fixed_quote()).unwrap();
    assert_eq!(hash, hash_by_hand(&fixed_quote()));
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
    let (pic, canister, admin) = with_roles();
    let before = events(&pic, canister).len();
    let next = Principal::from_slice(&[4; 29]);
    set_roles(&pic, canister, admin, next, watcher()).unwrap();
    let logged = events(&pic, canister);
    assert_eq!(logged.len(), before + 1, "one rotation, one event");
    assert_eq!(
        logged.last().unwrap().payload,
        EventType::RolesChanged {
            quoter: next.to_text(),
            watcher: watcher().to_text(),
        }
    );
}

/// A refused rotation must leave no audit line either: the event and the cell move
/// together or not at all.
#[test]
fn a_refused_set_roles_writes_no_event() {
    let (pic, canister, admin) = setup();
    let before = events(&pic, canister);
    assert!(set_roles(&pic, canister, stranger(), quoter(), watcher()).is_err());
    assert!(set_roles(&pic, canister, admin, Principal::anonymous(), watcher()).is_err());
    assert_eq!(events(&pic, canister), before);
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
    assert!(
        matches!(err, RegisterQuoteError::Expired { expires_at_s, .. } if expires_at_s == stale.expires_at_s),
        "say why: {err:?}"
    );

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
        matches!(
            err,
            RegisterQuoteError::ExpiresTooFarAhead {
                max_lifetime_s: MAX_QUOTE_LIFETIME_S,
                ..
            }
        ),
        "say why: {err:?}"
    );
}

/// The byte cap through the endpoint: one byte over is refused naming the field and stores
/// nothing, and the cap itself registers.
#[test]
fn register_quote_refuses_a_string_over_the_byte_cap() {
    let (pic, canister, _) = with_roles();
    let over = Quote {
        dst_address: "a".repeat(MAX_TEXT_BYTES + 1),
        ..fixed_quote()
    };
    let err = register_quote(&pic, canister, quoter(), &over).expect_err("one byte over the cap");
    assert_eq!(
        err,
        RegisterQuoteError::InvalidQuote(QuoteError::TextTooLong {
            field: "dst_address".to_string(),
            len: MAX_TEXT_BYTES as u64 + 1
        }),
        "name the field"
    );
    // refused at conversion, so the canister never computed an id for it: the id it would
    // have is built by hand, and the builder is checked against the golden first
    assert!(types::Quote::try_from(over.clone()).is_err());
    assert_eq!(hash_by_hand(&golden_quote()), golden_hash());
    assert_eq!(
        get_pending(&pic, canister, quoter(), hash_by_hand(&over)).unwrap(),
        None,
        "and nothing was stored under the id it would have had"
    );

    let at_cap = Quote {
        dst_address: "a".repeat(MAX_TEXT_BYTES),
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

/// Everything the canister holds is in stable memory: the roles and the pending store both
/// come through an upgrade untouched.
#[test]
fn roles_and_the_pending_store_survive_an_upgrade() {
    let (pic, canister, admin) = with_roles();
    let hash = register_quote(&pic, canister, quoter(), &fixed_quote()).unwrap();

    upgrade(&pic, canister, admin).unwrap();

    assert_eq!(
        get_pending(&pic, canister, quoter(), hash).unwrap(),
        Some(fixed_quote()),
        "the pending store is stable, so the quote is still there"
    );
    // still the quoter, without a second set_roles, and a re-registration is still fine
    assert_eq!(
        register_quote(&pic, canister, quoter(), &fixed_quote()).unwrap(),
        hash
    );
    assert!(register_quote(&pic, canister, stranger(), &fixed_quote()).is_err());
}

/// The pending store survives upgrades, so a store a compromised quoter filled is cleared in
/// place. Only a controller may: the quoter, the watcher and a stranger are refused and the
/// store is untouched. A clear answers how many quotes went, writes no event, and the
/// quoter registers again after it.
#[test]
fn only_a_controller_clears_the_pending_store_and_registration_works_after() {
    let (pic, canister, admin) = with_roles();
    let hash = register_quote(&pic, canister, quoter(), &fixed_quote()).unwrap();
    let other = Quote {
        nonce: 8,
        ..fixed_quote()
    };
    let other_hash = register_quote(&pic, canister, quoter(), &other).unwrap();
    let before = event_count(&pic, canister);

    for caller in [quoter(), watcher(), stranger()] {
        assert_eq!(
            clear_pending_quotes(&pic, canister, caller),
            Err(GuardError::NotController)
        );
    }
    assert_eq!(
        get_pending(&pic, canister, quoter(), hash).unwrap(),
        Some(fixed_quote()),
        "a refused clear leaves the store alone"
    );

    assert_eq!(clear_pending_quotes(&pic, canister, admin), Ok(2));
    for gone in [hash, other_hash] {
        assert_eq!(get_pending(&pic, canister, quoter(), gone).unwrap(), None);
    }
    assert_eq!(event_count(&pic, canister), before, "pre-money: no event");

    assert_eq!(
        register_quote(&pic, canister, quoter(), &fixed_quote()),
        Ok(hash)
    );
    assert_eq!(clear_pending_quotes(&pic, canister, admin), Ok(1));
}
