use super::*;
use settlement_api::types::quote::{GasMode, MAX_QUOTE_STRING_BYTES};

// A copy of the fixture in settlement_api's `types/quote/tests.rs`: a cfg(test) helper
// cannot cross crates.
/// The cross-repo fixture: swapic-backend's mirror builds this same quote field for
/// field and must hash it to the same golden line.
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

/// A live quote and the clock it is live on. Every store test calls `clear_pending`
/// first, because a single-threaded harness gives them all one map.
fn pending_quote(nonce: u64) -> Quote {
    Quote {
        nonce,
        ..fixed_quote()
    }
}

fn just_before_expiry(q: &Quote) -> u64 {
    q.expires_at_s - 1
}

#[test]
fn register_refuses_a_quote_that_has_already_expired() {
    clear_pending();
    let q = pending_quote(9_001);
    let err = register(q.clone(), q.expires_at_s + 1).expect_err("expired quotes are refused");
    assert!(err.contains("expired"), "say why: {err}");
    assert_eq!(get_pending(&quote_hash(&q)), None, "and nothing was stored");
}

#[test]
fn register_refuses_a_quote_that_does_not_validate() {
    clear_pending();
    let q = Quote {
        refund_address: Some(String::new()),
        ..pending_quote(9_002)
    };
    assert!(register(q.clone(), just_before_expiry(&q)).is_err());
    assert_eq!(get_pending(&quote_hash(&q)), None);
}

#[test]
fn register_refuses_an_oversized_string_and_takes_one_at_the_cap() {
    clear_pending();
    type SetField = fn(&mut Quote, String);
    let fields: [(&str, SetField); 5] = [
        ("src_token", |q, s| q.src_token = s),
        ("dst_token", |q, s| q.dst_token = s),
        ("dst_address", |q, s| q.dst_address = s),
        ("rail", |q, s| q.rail = s),
        ("refund_address", |q, s| q.refund_address = Some(s)),
    ];
    for (field, set) in fields {
        let mut over = pending_quote(9_006);
        set(&mut over, "a".repeat(MAX_QUOTE_STRING_BYTES + 1));
        let err =
            register(over.clone(), just_before_expiry(&over)).expect_err("one byte over the cap");
        assert!(err.contains(field), "name the field: {err}");
        assert_eq!(
            get_pending(&quote_hash(&over)),
            None,
            "and nothing was stored"
        );

        let mut at_cap = pending_quote(9_006);
        set(&mut at_cap, "a".repeat(MAX_QUOTE_STRING_BYTES));
        assert!(
            register(at_cap.clone(), just_before_expiry(&at_cap)).is_ok(),
            "{field} at the cap is allowed"
        );
    }

    // bytes, not chars: 129 two-byte chars is under the cap in chars and over it in bytes
    let wide = Quote {
        dst_address: "é".repeat(129),
        ..pending_quote(9_007)
    };
    assert!(register(wide.clone(), just_before_expiry(&wide)).is_err());
}

#[test]
fn register_stores_the_quote_under_its_hash_and_a_rerun_overwrites() {
    clear_pending();
    let q = pending_quote(9_003);
    let h = register(q.clone(), just_before_expiry(&q)).expect("a live quote registers");
    assert_eq!(h, quote_hash(&q));
    assert_eq!(get_pending(&h), Some(q.clone()));
    // the quoter re-registers after an upgrade, so the same quote twice is not an error
    assert_eq!(register(q.clone(), just_before_expiry(&q)), Ok(h));
    assert_eq!(get_pending(&h), Some(q));
    assert_eq!(get_pending(&[0; 32]), None);
}

/// The boundary the expiry check draws: `expires_at_s` is the last second the quote is
/// still good.
#[test]
fn a_quote_is_live_up_to_and_including_its_expiry_second() {
    clear_pending();
    let q = pending_quote(9_004);
    assert!(register(q.clone(), q.expires_at_s).is_ok());
    assert!(register(q.clone(), q.expires_at_s + 1).is_err());
}

/// An immortal quote would hold a slot against the cap forever, so the far end of the
/// window is bounded as well as the near one.
#[test]
fn register_refuses_a_quote_that_expires_too_far_ahead() {
    clear_pending();
    let q = pending_quote(9_005);
    let far = q.expires_at_s - MAX_QUOTE_LIFETIME_S - 1;
    let err = register(q.clone(), far).expect_err("that is too long to hold a slot");
    assert!(
        err.contains(&MAX_QUOTE_LIFETIME_S.to_string()),
        "say why: {err}"
    );
    assert_eq!(get_pending(&quote_hash(&q)), None);
    // and the edge of the window is inside it
    assert!(register(q.clone(), far + 1).is_ok());
}

/// The backstop against a looping quoter: a full store refuses a new quote but must
/// still take a re-registration, which is how the quoter replays after an upgrade.
#[test]
fn a_full_store_refuses_a_new_quote_and_still_takes_a_repeat() {
    clear_pending();
    // small quotes, distinct only by nonce, so the fill is cheap
    let tiny = |nonce: u64| Quote {
        src_token: String::new(),
        dst_token: String::new(),
        dst_address: String::new(),
        rail: String::new(),
        nonce,
        ..fixed_quote()
    };
    let now = just_before_expiry(&tiny(0));
    for nonce in 0..MAX_PENDING as u64 {
        register(tiny(nonce), now).expect("fills to the cap");
    }

    let overflow = tiny(MAX_PENDING as u64);
    let err = register(overflow.clone(), now).expect_err("the store is full");
    assert!(err.contains("full"), "say why: {err}");
    assert_eq!(get_pending(&quote_hash(&overflow)), None);

    // the one thing a full store must still accept
    let repeat = tiny(0);
    assert_eq!(register(repeat.clone(), now), Ok(quote_hash(&repeat)));
}
