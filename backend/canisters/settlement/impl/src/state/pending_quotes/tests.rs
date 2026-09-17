use super::*;
use std::time::Duration;
use types::address::MAX_TEXT_BYTES;
use types::rail::UnknownRail;
use types::{ChainId, GasMode, Rail, TokenAmount};

// A copy of the fixture in the types crate's `quote/tests.rs`: a cfg(test) helper
// cannot cross crates.
/// The cross-repo fixture: swapic-backend's mirror builds this same quote field for
/// field and must hash it to the same golden line.
fn fixed_quote() -> Quote {
    Quote {
        version: 1,
        src_chain: ChainId::BASE,
        src_token: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"
            .parse()
            .unwrap(),
        amount_in: TokenAmount::from(25_000_000_u32),
        dst_chain: ChainId::ARBITRUM,
        dst_token: "0xaf88d065e77c8cC2239327C5EDb3A432268e5831"
            .parse()
            .unwrap(),
        expected_out: TokenAmount::from(24_990_000_u32),
        min_out: TokenAmount::from(24_900_000_u32),
        dst_address: "0x7551A66653f9a20979ed81835a0b7008EC83401b"
            .parse()
            .unwrap(),
        refund_address: None,
        auto_refund: true,
        gas_mode: GasMode::Legacy,
        rail: Rail::CctpV2Fast,
        expires_at: UnixSeconds::new(1_800_000_000),
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

fn seconds_after(q: &Quote, secs: u64) -> UnixSeconds {
    UnixSeconds::new(q.expires_at.get() + secs)
}

fn seconds_before(q: &Quote, secs: u64) -> UnixSeconds {
    UnixSeconds::new(q.expires_at.get() - secs)
}

fn just_before_expiry(q: &Quote) -> UnixSeconds {
    seconds_before(q, 1)
}

#[test]
fn register_refuses_a_quote_that_has_already_expired() {
    clear_pending();
    let q = pending_quote(9_001);
    let now = seconds_after(&q, 1);
    assert_eq!(
        register(q.clone(), now),
        Err(RegisterError::Expired {
            expires_at: q.expires_at,
            now
        })
    );
    assert_eq!(
        get_pending(&q.hash().unwrap()),
        None,
        "and nothing was stored"
    );
}

#[test]
fn register_refuses_a_quote_that_does_not_validate() {
    clear_pending();
    let q = Quote {
        refund_address: Some("".parse().unwrap()),
        ..pending_quote(9_002)
    };
    assert_eq!(
        register(q.clone(), just_before_expiry(&q)),
        Err(RegisterError::InvalidQuote(QuoteError::EmptyRefundAddress))
    );
    assert_eq!(get_pending(&q.hash().unwrap()), None);
}

/// Text reaches the store through the wire conversion, which is where the byte cap is
/// enforced. A rail is a closed set rather than capped text, so any text that is not a
/// rail id is refused, at the cap or over it.
#[test]
fn register_refuses_an_oversized_string_and_takes_one_at_the_cap() {
    use settlement_api::types::quote::Quote as WireQuote;

    clear_pending();
    let wire = |nonce| WireQuote::from(pending_quote(nonce));
    type SetField = fn(&mut WireQuote, String);
    let fields: [(&str, SetField); 4] = [
        ("src_token", |q, s| q.src_token = s),
        ("dst_token", |q, s| q.dst_token = s),
        ("dst_address", |q, s| q.dst_address = s),
        ("refund_address", |q, s| q.refund_address = Some(s)),
    ];
    for (field, set) in fields {
        let mut over = wire(9_006);
        set(&mut over, "a".repeat(MAX_TEXT_BYTES + 1));
        assert_eq!(
            Quote::try_from(over),
            Err(QuoteError::TextTooLong {
                field,
                len: MAX_TEXT_BYTES + 1
            }),
            "one byte over the cap"
        );

        let mut at_cap = wire(9_006);
        set(&mut at_cap, "a".repeat(MAX_TEXT_BYTES));
        let at_cap = Quote::try_from(at_cap).expect("the cap itself converts");
        let now = just_before_expiry(&at_cap);
        assert!(
            register(at_cap, now).is_ok(),
            "{field} at the cap is allowed"
        );
    }

    for rail in ["a".repeat(MAX_TEXT_BYTES + 1), "a".repeat(MAX_TEXT_BYTES)] {
        let mut not_a_rail = wire(9_006);
        not_a_rail.rail = rail.clone();
        assert_eq!(
            Quote::try_from(not_a_rail),
            Err(QuoteError::UnknownRail(UnknownRail(rail)))
        );
    }

    // bytes, not chars: 129 two-byte chars is under the cap in chars and over it in bytes
    let mut wide = wire(9_007);
    wide.dst_address = "é".repeat(129);
    assert!(Quote::try_from(wide).is_err());
}

#[test]
fn register_stores_the_quote_under_its_hash_and_a_rerun_overwrites() {
    clear_pending();
    let q = pending_quote(9_003);
    let h = register(q.clone(), just_before_expiry(&q)).expect("a live quote registers");
    assert_eq!(h, q.hash().unwrap());
    assert_eq!(get_pending(&h), Some(q.clone()));
    // the quoter re-registers after an upgrade, so the same quote twice is not an error
    assert_eq!(register(q.clone(), just_before_expiry(&q)), Ok(h));
    assert_eq!(get_pending(&h), Some(q));
    assert_eq!(get_pending(&QuoteHash::new([0; 32])), None);
}

/// The boundary the expiry check draws: `expires_at` is the last second the quote is
/// still good.
#[test]
fn a_quote_is_live_up_to_and_including_its_expiry_second() {
    clear_pending();
    let q = pending_quote(9_004);
    assert!(register(q.clone(), q.expires_at).is_ok());
    assert!(register(q.clone(), seconds_after(&q, 1)).is_err());
}

/// An immortal quote would hold a slot against the cap forever, so the far end of the
/// window is bounded as well as the near one.
#[test]
fn register_refuses_a_quote_that_expires_too_far_ahead() {
    clear_pending();
    let q = pending_quote(9_005);
    let far = seconds_before(&q, MAX_QUOTE_LIFETIME.as_secs() + 1);
    assert_eq!(
        register(q.clone(), far),
        Err(RegisterError::ExpiresTooFarAhead {
            expires_at: q.expires_at,
            now: far
        })
    );
    assert_eq!(get_pending(&q.hash().unwrap()), None);
    // and the edge of the window is inside it
    assert!(register(q.clone(), seconds_before(&q, MAX_QUOTE_LIFETIME.as_secs())).is_ok());
}

/// The backstop against a looping quoter: a full store refuses a new quote but must
/// still take a re-registration of one it holds.
#[test]
fn a_full_store_refuses_a_new_quote_and_still_takes_a_repeat() {
    clear_pending();
    // small quotes, distinct only by nonce, so the fill is cheap
    let tiny = |nonce: u64| Quote {
        src_token: "a".parse().unwrap(),
        dst_token: "b".parse().unwrap(),
        dst_address: "c".parse().unwrap(),
        rail: Rail::Eco,
        nonce,
        ..fixed_quote()
    };
    let now = just_before_expiry(&tiny(0));
    for nonce in 0..MAX_PENDING {
        register(tiny(nonce), now).expect("fills to the cap");
    }

    let overflow = tiny(MAX_PENDING);
    assert_eq!(
        register(overflow.clone(), now),
        Err(RegisterError::StoreFull)
    );
    assert_eq!(get_pending(&overflow.hash().unwrap()), None);

    // the one thing a full store must still accept
    let repeat = tiny(0);
    assert_eq!(register(repeat.clone(), now), Ok(repeat.hash().unwrap()));
}

/// A permit deadline that reaches past the last representable second never closes.
#[test]
fn sweep_keeps_a_quote_whose_permit_window_never_closes() {
    clear_pending();
    let q = Quote {
        expires_at: UnixSeconds::new(u64::MAX - 10),
        ..pending_quote(9_008)
    };
    register(q.clone(), UnixSeconds::new(u64::MAX - 20)).expect("a live quote registers");
    assert_eq!(
        sweep_expired(UnixSeconds::new(u64::MAX), Duration::from_secs(120)),
        0
    );
    assert!(get_pending(&q.hash().unwrap()).is_some());
}
