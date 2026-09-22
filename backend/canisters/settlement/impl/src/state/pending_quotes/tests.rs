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
        // the store takes only a quote a refund can be paid on, so the fixture names one;
        // the cross-repo hash vector is the types crate's, which keeps the absent field
        refund_address: Some(
            "0x7551A66653f9a20979ed81835a0b7008EC83401b"
                .parse()
                .unwrap(),
        ),
        auto_refund: true,
        gas_mode: GasMode::Legacy,
        rail: Rail::CctpV2Fast,
        expires_at: UnixSeconds::new(1_800_000_000),
        nonce: 7,
    }
}

/// A live quote and the clock it is live on. Every store test calls `clear` first,
/// because a single-threaded harness gives them all one map.
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

/// A quote nobody could be refunded on is one whose swap could only freeze with the
/// user's funds in the vault, so the quoter learns at registration and not after the
/// money has arrived.
#[test]
fn register_refuses_a_quote_that_names_no_refund_address() {
    clear();
    let none = Quote {
        refund_address: None,
        ..pending_quote(9_010)
    };
    assert_eq!(
        register(none.clone(), just_before_expiry(&none)),
        Err(RegisterError::NoRefundAddress)
    );
    let not_an_address = Quote {
        refund_address: Some("0xrefund".parse().unwrap()),
        ..pending_quote(9_011)
    };
    assert_eq!(
        register(not_an_address.clone(), just_before_expiry(&not_an_address)),
        Err(RegisterError::RefundAddressNotAnAddress {
            reason: types::evm::EvmAddressError::WrongLength { len: 6 },
        })
    );
    assert!(all_pending().is_empty(), "nothing was stored");
    let good = pending_quote(9_012);
    assert!(register(good.clone(), just_before_expiry(&good)).is_ok());
}

#[test]
fn register_refuses_a_quote_that_has_already_expired() {
    clear();
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
    clear();
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

    clear();
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
        let registered = register(at_cap, now);
        // text at the cap crosses the wire; the refund address is held to more than a
        // length, because a refund has to be payable to it
        if field == "refund_address" {
            assert!(
                matches!(
                    registered,
                    Err(RegisterError::RefundAddressNotAnAddress { .. })
                ),
                "{field} at the cap is text, and a refund address must be an address: \
                 {registered:?}"
            );
        } else {
            assert!(registered.is_ok(), "{field} at the cap is allowed");
        }
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
    clear();
    let q = pending_quote(9_003);
    let h = register(q.clone(), just_before_expiry(&q)).expect("a live quote registers");
    assert_eq!(h, q.hash().unwrap());
    assert_eq!(get_pending(&h), Some(q.clone()));
    // the store is stable and keeps a bare `Quote`, so an upgrade loses nothing and a repeat
    // is the quoter retrying: the same quote twice overwrites itself, and is not an error
    assert_eq!(register(q.clone(), just_before_expiry(&q)), Ok(h));
    assert_eq!(get_pending(&h), Some(q));
    assert_eq!(get_pending(&QuoteHash::new([0; 32])), None);
}

/// The boundary the expiry check draws: `expires_at` is the last second the quote is
/// still good.
#[test]
fn a_quote_is_live_up_to_and_including_its_expiry_second() {
    clear();
    let q = pending_quote(9_004);
    assert!(register(q.clone(), q.expires_at).is_ok());
    assert!(register(q.clone(), seconds_after(&q, 1)).is_err());
}

/// An immortal quote would hold a slot against the cap forever, so the far end of the
/// window is bounded as well as the near one.
#[test]
fn register_refuses_a_quote_that_expires_too_far_ahead() {
    clear();
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

/// Small quotes, distinct only by nonce, so a fill to the cap is cheap.
fn tiny(nonce: u64) -> Quote {
    Quote {
        src_token: "a".parse().unwrap(),
        dst_token: "b".parse().unwrap(),
        dst_address: "c".parse().unwrap(),
        rail: Rail::Eco,
        nonce,
        ..fixed_quote()
    }
}

fn fill_to_the_cap() -> UnixSeconds {
    let now = just_before_expiry(&tiny(0));
    for nonce in 0..MAX_PENDING {
        register(tiny(nonce), now).expect("fills to the cap");
    }
    now
}

/// A small quote expiring at `expires_at`, so a fixture can lay quotes out along the index.
fn expiring(nonce: u64, expires_at: u64) -> Quote {
    Quote {
        expires_at: UnixSeconds::new(expires_at),
        ..tiny(nonce)
    }
}

/// The expiry index as a full scan of the store would build it.
fn scanned_index() -> Vec<ExpiryKey> {
    let mut keys: Vec<ExpiryKey> = all_pending()
        .into_iter()
        .map(|(quote_hash, quote)| ExpiryKey {
            expires_at: quote.expires_at,
            quote_hash,
        })
        .collect();
    keys.sort();
    keys
}

/// What the cap has to bound: a full store of quotes nobody can evict yet. The pass walks
/// the index in expiry order, so it reads the soonest expiry, sees the window is open and
/// stops, instead of decoding ten thousand quotes to evict none of them.
#[test]
fn a_pass_over_a_full_store_of_live_quotes_reads_one_entry_and_evicts_nothing() {
    clear();
    let cap = 200;
    let now = fill_to_the_cap();

    let swept = sweep_expired(now, Duration::from_secs(120), cap);
    assert!(
        swept.visited <= cap,
        "the cap bounds the work: {} entries read",
        swept.visited
    );
    assert_eq!(
        swept,
        Evicted {
            dropped: 0,
            visited: 1,
            more: false
        }
    );
    assert_eq!(all_pending().len(), MAX_PENDING as usize);
    assert_eq!(expiry_index().len(), MAX_PENDING as usize);
}

/// And with stale quotes at the head of the index the same pass evicts its cap, reads one
/// entry past it to learn that work remains, and never reaches the live quotes behind them.
#[test]
fn a_pass_evicts_its_cap_from_the_head_and_stops_at_the_first_live_quote() {
    clear();
    let cap: usize = 20;
    let stale: usize = 50;
    let base = 1_800_000_000;
    let now = UnixSeconds::new(base);
    let window = Duration::from_secs(120);
    for nonce in 0..stale as u64 {
        register(expiring(nonce, base + 10), now).expect("a live quote registers");
    }
    for nonce in 0..1_000 {
        register(expiring(1_000 + nonce, base + 80_000), now).expect("a live quote registers");
    }

    let after = UnixSeconds::new(base + 10 + window.as_secs() + 1);
    assert_eq!(
        sweep_expired(after, window, cap),
        Evicted {
            dropped: cap,
            visited: cap + 1,
            more: true
        }
    );
    assert_eq!(all_pending().len(), 1_050 - cap);

    // the rest of the stale head drains over the next passes, and then the pass stops at
    // the first quote whose window is still open, whatever sits behind it
    for expected in [cap, stale - 2 * cap] {
        assert_eq!(sweep_expired(after, window, cap).dropped, expected);
    }
    assert_eq!(
        sweep_expired(after, window, cap),
        Evicted {
            dropped: 0,
            visited: 1,
            more: false
        },
        "the live quotes behind the head cost one entry, not a thousand"
    );
    assert_eq!(all_pending().len(), 1_000);
}

/// The index is the store keyed by expiry and nothing else, at every point the store moves:
/// a registration, a repeat of one already held, an eviction, a clear.
#[test]
fn the_expiry_index_matches_a_full_scan_of_the_store() {
    clear();
    assert_eq!(expiry_index(), scanned_index());
    let base = 1_800_000_000;
    let now = UnixSeconds::new(base);
    for (nonce, expires_at) in [
        (1, base + 5),
        (2, base + 5),
        (3, base + 50),
        (4, base + 900),
    ] {
        register(expiring(nonce, expires_at), now).expect("a live quote registers");
        assert_eq!(expiry_index(), scanned_index(), "after registering {nonce}");
    }
    assert_eq!(expiry_index().len(), 4);

    // a registration the store refuses indexes nothing: the insert is after every rule
    assert!(register(expiring(5, base - 1), now).is_err());
    assert_eq!(
        expiry_index(),
        scanned_index(),
        "after a refused registration"
    );
    assert_eq!(expiry_index().len(), 4);

    // the hash covers `expires_at`, so a repeat is the same key written again
    register(expiring(3, base + 50), now).expect("a repeat registers");
    assert_eq!(expiry_index(), scanned_index(), "after a repeat");
    assert_eq!(expiry_index().len(), 4);

    let window = Duration::from_secs(120);
    let swept = sweep_expired(UnixSeconds::new(base + 5 + window.as_secs() + 1), window, 2);
    assert_eq!((swept.dropped, swept.more), (2, false));
    assert_eq!(expiry_index(), scanned_index(), "after an eviction");
    assert_eq!(expiry_index().len(), 2);

    assert_eq!(clear(), 2);
    assert_eq!(expiry_index(), scanned_index(), "after a clear");
    assert!(expiry_index().is_empty());
}

/// The backstop against a looping quoter: a full store refuses a new quote but must
/// still take a re-registration of one it holds.
#[test]
fn a_full_store_refuses_a_new_quote_and_still_takes_a_repeat() {
    clear();
    let now = fill_to_the_cap();

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
    clear();
    let q = Quote {
        expires_at: UnixSeconds::new(u64::MAX - 10),
        ..pending_quote(9_008)
    };
    register(q.clone(), UnixSeconds::new(u64::MAX - 20)).expect("a live quote registers");
    assert_eq!(
        sweep_expired(UnixSeconds::new(u64::MAX), Duration::from_secs(120), 200),
        Evicted {
            dropped: 0,
            visited: 1,
            more: false
        },
        "the pass reads the soonest expiry and stops there"
    );
    assert!(get_pending(&q.hash().unwrap()).is_some());
}

/// One pass evicts at most its cap, so a store full of stale quotes cannot put every
/// removal in one message. The passes after it drain the rest.
#[test]
fn sweep_evicts_at_most_the_cap_and_says_that_work_remains() {
    clear();
    let q = tiny(0);
    let now = just_before_expiry(&q);
    for nonce in 0..10 {
        register(tiny(nonce), now).expect("a live quote registers");
    }
    let stale = seconds_after(&q, 121);
    let window = Duration::from_secs(120);

    // one entry past the cap is read, and it is the evidence that work remains
    assert_eq!(
        sweep_expired(stale, window, 4),
        Evicted {
            dropped: 4,
            visited: 5,
            more: true
        }
    );
    assert_eq!(
        sweep_expired(stale, window, 4),
        Evicted {
            dropped: 4,
            visited: 5,
            more: true
        }
    );
    assert_eq!(
        sweep_expired(stale, window, 4),
        Evicted {
            dropped: 2,
            visited: 2,
            more: false
        },
        "the last pass drains the store and reports nothing left"
    );
    assert_eq!(get_pending(&q.hash().unwrap()), None);
}

/// The recovery for a store a looping or compromised quoter filled, which an upgrade no
/// longer empties: a clear answers how many quotes went, and the quoter registers again.
#[test]
fn clear_empties_a_full_store_and_registration_works_again() {
    clear();
    let now = fill_to_the_cap();
    let next = tiny(MAX_PENDING);
    assert_eq!(register(next.clone(), now), Err(RegisterError::StoreFull));

    assert_eq!(clear(), MAX_PENDING);
    assert_eq!(get_pending(&tiny(0).hash().unwrap()), None);
    assert_eq!(register(next.clone(), now), Ok(next.hash().unwrap()));
    assert_eq!(clear(), 1);
    assert_eq!(clear(), 0, "an empty store clears to nothing");
}
