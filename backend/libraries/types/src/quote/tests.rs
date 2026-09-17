use super::*;

fn text<T: std::str::FromStr>(s: &str) -> T
where
    T::Err: std::fmt::Debug,
{
    s.parse().unwrap()
}

/// The cross-repo fixture: swapic-backend's mirror builds this same quote field for
/// field and must hash it to the same golden line.
fn fixed_quote() -> Quote {
    Quote {
        version: 1,
        src_chain: ChainId::BASE,
        src_token: text("0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"),
        amount_in: TokenAmount::from(25_000_000_u32),
        dst_chain: ChainId::ARBITRUM,
        dst_token: text("0xaf88d065e77c8cC2239327C5EDb3A432268e5831"),
        expected_out: TokenAmount::from(24_990_000_u32),
        min_out: TokenAmount::from(24_900_000_u32),
        dst_address: text("0x7551A66653f9a20979ed81835a0b7008EC83401b"),
        refund_address: None,
        auto_refund: true,
        gas_mode: GasMode::Legacy,
        rail: Rail::CctpV2Fast,
        expires_at: UnixSeconds::new(1_800_000_000),
        nonce: 7,
    }
}

/// `fixed_quote` with exactly one field moved, once per field. Doubles as the
/// round-trip sample set.
fn one_field_changed() -> Vec<(&'static str, Quote)> {
    let q = fixed_quote();
    // exhaustive destructure: a new field breaks this line, so the list below cannot
    // silently miss one and leave `QUOTE_FIELD_COUNT` undercounting
    let Quote {
        version: _,
        src_chain: _,
        src_token: _,
        amount_in: _,
        dst_chain: _,
        dst_token: _,
        expected_out: _,
        min_out: _,
        dst_address: _,
        refund_address: _,
        auto_refund: _,
        gas_mode: _,
        rail: _,
        expires_at: _,
        nonce: _,
    } = &q;
    vec![
        (
            "version",
            Quote {
                version: 2,
                ..q.clone()
            },
        ),
        (
            "src_chain",
            Quote {
                src_chain: ChainId::ETHEREUM,
                ..q.clone()
            },
        ),
        (
            "src_token",
            Quote {
                src_token: text("0xdAC17F958D2ee523a2206206994597C13D831ec7"),
                ..q.clone()
            },
        ),
        (
            "amount_in",
            Quote {
                amount_in: TokenAmount::from(25_000_001_u32),
                ..q.clone()
            },
        ),
        (
            "dst_chain",
            Quote {
                dst_chain: ChainId::ETHEREUM,
                ..q.clone()
            },
        ),
        (
            "dst_token",
            Quote {
                dst_token: text("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
                ..q.clone()
            },
        ),
        (
            "expected_out",
            Quote {
                expected_out: TokenAmount::from(24_990_001_u32),
                ..q.clone()
            },
        ),
        (
            "min_out",
            Quote {
                min_out: TokenAmount::from(24_900_001_u32),
                ..q.clone()
            },
        ),
        (
            "dst_address",
            Quote {
                dst_address: text("0x1111111254EEB25477B68fb85Ed929f73A960582"),
                ..q.clone()
            },
        ),
        (
            // a non-empty address, deliberately: None and Some("") share a preimage
            // by design, which `none_and_empty_refund_address_share_a_preimage` pins
            "refund_address",
            Quote {
                refund_address: Some(text("0x1111111254EEB25477B68fb85Ed929f73A960582")),
                ..q.clone()
            },
        ),
        (
            "auto_refund",
            Quote {
                auto_refund: false,
                ..q.clone()
            },
        ),
        (
            "gas_mode",
            Quote {
                gas_mode: GasMode::Gasless,
                ..q.clone()
            },
        ),
        (
            "rail",
            Quote {
                rail: Rail::CctpV2Standard,
                ..q.clone()
            },
        ),
        (
            "expires_at",
            Quote {
                expires_at: UnixSeconds::new(1_800_000_001),
                ..q.clone()
            },
        ),
        ("nonce", Quote { nonce: 8, ..q }),
    ]
}

/// The awkward shapes: empty text, multibyte utf8, and the top of every integer.
fn edge_quotes() -> Vec<Quote> {
    vec![
        Quote {
            src_token: text(""),
            dst_token: text(""),
            dst_address: text(""),
            refund_address: None,
            rail: Rail::Eco,
            ..fixed_quote()
        },
        Quote {
            // length is bytes, not chars
            src_token: text("tökén \u{2603}"),
            dst_address: text("\u{1F680}"),
            refund_address: Some(text("réfund")),
            rail: Rail::CctpV2Standard,
            ..fixed_quote()
        },
        Quote {
            version: u8::MAX,
            src_chain: ChainId::new(u64::MAX),
            // high words set: proves the u128s are 16 bytes, not truncated u64s
            amount_in: TokenAmount::from(u128::MAX),
            dst_chain: ChainId::new(u64::MAX),
            expected_out: TokenAmount::from(u128::MAX - 1),
            min_out: TokenAmount::from(1u128 << 70),
            auto_refund: false,
            gas_mode: GasMode::Gasless,
            expires_at: UnixSeconds::new(u64::MAX),
            nonce: u64::MAX,
            ..fixed_quote()
        },
    ]
}

const GOLDEN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/golden/quote_hash_v1.txt");

#[test]
fn any_field_change_changes_the_hash() {
    let base = fixed_quote().hash().unwrap();
    let changed = one_field_changed();
    assert_eq!(
        changed.len(),
        QUOTE_FIELD_COUNT,
        "one_field_changed() must move every field"
    );
    let mut seen = std::collections::BTreeSet::new();
    for (field, q) in changed {
        let h = q.hash().unwrap();
        assert_ne!(base, h, "moving {field} left the hash alone");
        assert!(seen.insert(h), "two different quotes hash alike ({field})");
    }
}

/// The layout is fixed-width apart from the four strings, so the total is arithmetic:
/// a stray or missing prefix shows up here before it reaches the golden file.
#[test]
fn the_preimage_is_exactly_as_long_as_the_layout_says() {
    // 1 version + 8 src_chain + (4+42) src_token + 16 amount_in + 8 dst_chain
    // + (4+42) dst_token + 16 expected_out + 16 min_out + (4+42) dst_address
    // + (4+0) refund_address + 1 auto_refund + 1 gas_mode + (4+12) rail
    // + 8 expires_at + 8 nonce
    assert_eq!(fixed_quote().canonical_bytes().unwrap().len(), 241);
}

#[test]
fn gas_mode_encodes_one_byte() {
    let gasless = Quote {
        gas_mode: GasMode::Gasless,
        ..fixed_quote()
    }
    .canonical_bytes()
    .unwrap();
    let legacy = fixed_quote().canonical_bytes().unwrap();
    assert_eq!(gasless.len(), legacy.len(), "one byte either way");
    // the byte sits after the refund_address prefix, before the rail
    let at = gasless.len() - 8 - 8 - (4 + 12) - 1;
    assert_eq!(gasless[at], 0, "Gasless encodes 0");
    assert_eq!(legacy[at], 1, "Legacy encodes 1");
}

/// The one place the layout is not injective, pinned here so nobody discovers it by
/// accident: an absent refund address and an empty one write the same four bytes.
/// `validate` refuses the empty one, so the collision cannot reach a stored quote.
#[test]
fn none_and_empty_refund_address_share_a_preimage() {
    let none = fixed_quote();
    let empty = Quote {
        refund_address: Some(text("")),
        ..fixed_quote()
    };
    assert_eq!(none.canonical_bytes(), empty.canonical_bytes());
    none.validate().expect("an absent refund address is fine");
    assert_eq!(empty.validate(), Err(QuoteError::EmptyRefundAddress));
    // and the reader resolves it the one way that survives a round trip
    assert_eq!(
        Quote::parse(&none.canonical_bytes().unwrap()).unwrap(),
        none
    );
}

/// A quote that names no token or no destination cannot be settled, so it never takes a
/// pending slot. An absent refund address stays legal: it has a meaning of its own.
#[test]
fn validate_refuses_empty_text_naming_the_field() {
    type SetField = fn(&mut Quote);
    let fields: [(&str, SetField); 3] = [
        ("src_token", |q| q.src_token = text("")),
        ("dst_token", |q| q.dst_token = text("")),
        ("dst_address", |q| q.dst_address = text("")),
    ];
    for (field, empty) in fields {
        let mut quote = fixed_quote();
        empty(&mut quote);
        assert_eq!(quote.validate(), Err(QuoteError::EmptyText { field }));
    }
}

/// An amount the preimage cannot hold is a typed refusal at validation, and hashing such a
/// quote is an error rather than a panic.
#[test]
fn validate_refuses_an_amount_above_u128_max_and_hash_does_not_panic() {
    let too_large = TokenAmount::from(u128::MAX)
        .checked_add(TokenAmount::ONE)
        .unwrap();
    type SetField = fn(&mut Quote, TokenAmount);
    let fields: [(&str, SetField); 3] = [
        ("amount_in", |q, a| q.amount_in = a),
        ("expected_out", |q, a| q.expected_out = a),
        ("min_out", |q, a| q.min_out = a),
    ];
    for (field, set) in fields {
        let mut quote = fixed_quote();
        set(&mut quote, too_large);
        assert_eq!(quote.validate(), Err(QuoteError::AmountTooLarge { field }));
        assert_eq!(quote.hash(), Err(CanonicalError::AmountTooLarge(too_large)));

        let mut at_max = fixed_quote();
        set(&mut at_max, TokenAmount::from(u128::MAX));
        assert_eq!(at_max.validate(), Ok(()), "{field} at u128::MAX");
        assert!(at_max.hash().is_ok());
    }
}

#[test]
fn validate_refuses_a_version_this_canister_cannot_read() {
    let quote = Quote {
        version: 2,
        ..fixed_quote()
    };
    assert_eq!(quote.validate(), Err(QuoteError::UnsupportedVersion(2)));
}

#[test]
fn parse_round_trips_every_field_variation() {
    let mut all = vec![fixed_quote()];
    all.extend(one_field_changed().into_iter().map(|(_, q)| q));
    all.extend(edge_quotes());
    for q in all {
        let bytes = q.canonical_bytes().unwrap();
        let back = Quote::parse(&bytes).unwrap_or_else(|e| panic!("{e} for {q:?}"));
        assert_eq!(back, q, "round trip lost a field");
        assert_eq!(
            back.canonical_bytes().unwrap(),
            bytes,
            "and the re-encode drifted"
        );
    }
}

#[test]
fn parse_rejects_a_truncated_preimage() {
    let bytes = fixed_quote().canonical_bytes().unwrap();
    for cut in [0, 1, 40, 100, bytes.len() - 1] {
        let err = Quote::parse(&bytes[..cut]).expect_err("truncated bytes are not a quote");
        assert!(
            matches!(err, QuoteError::Truncated { .. }),
            "say what went wrong: {err}"
        );
    }
}

#[test]
fn parse_rejects_trailing_bytes() {
    let mut bytes = fixed_quote().canonical_bytes().unwrap();
    bytes.push(0);
    assert_eq!(
        Quote::parse(&bytes),
        Err(QuoteError::TrailingBytes {
            consumed: 241,
            len: 242
        })
    );
}

#[test]
fn parse_rejects_bytes_outside_the_layout() {
    let base = fixed_quote().canonical_bytes().unwrap();
    let auto_refund_at = base.len() - 8 - 8 - (4 + 12) - 1 - 1;

    let mut bad_bool = base.clone();
    bad_bool[auto_refund_at] = 2;
    assert_eq!(
        Quote::parse(&bad_bool),
        Err(QuoteError::NotABool {
            field: "auto_refund",
            value: 2
        })
    );

    let mut bad_mode = base.clone();
    bad_mode[auto_refund_at + 1] = 2;
    assert_eq!(Quote::parse(&bad_mode), Err(QuoteError::NotAGasMode(2)));

    // the src_token length prefix, blown up past the end of the input
    let mut long_str = base.clone();
    long_str[9..13].copy_from_slice(&u32::MAX.to_be_bytes());
    assert!(Quote::parse(&long_str).is_err(), "a length must fit");

    // 0xff is never valid utf8, so this lands inside src_token's bytes
    let mut bad_utf8 = base;
    bad_utf8[13] = 0xff;
    assert_eq!(
        Quote::parse(&bad_utf8),
        Err(QuoteError::NotUtf8 { field: "src_token" })
    );
}

/// The rail is a closed set: text the writer could never have produced for a rail does
/// not parse, and the error names the field.
#[test]
fn parse_rejects_a_rail_that_is_not_one() {
    let mut bytes = fixed_quote().canonical_bytes().unwrap();
    // "cctp_v2_fast" becomes "cctp_v2_fasT", same length
    let rail_last = bytes.len() - 8 - 8 - 1;
    bytes[rail_last] = b'T';
    let err = Quote::parse(&bytes).unwrap_err();
    assert_eq!(
        err,
        QuoteError::UnknownRail(UnknownRail("cctp_v2_fasT".to_string()))
    );
    assert!(err.to_string().contains("rail"), "name the field: {err}");
}

/// Text past the cap never reaches a quote, from bytes either.
#[test]
fn parse_rejects_text_over_the_cap() {
    let long = Quote {
        dst_address: text(&"a".repeat(MAX_TEXT_BYTES)),
        ..fixed_quote()
    };
    let mut bytes = long.canonical_bytes().unwrap();
    // splice one more byte into dst_address and bump its length prefix
    let prefix_at = 1 + 8 + (4 + 42) + 16 + 8 + (4 + 42) + 16 + 16;
    bytes[prefix_at..prefix_at + 4].copy_from_slice(&257u32.to_be_bytes());
    bytes.insert(prefix_at + 4, b'a');
    assert_eq!(
        Quote::parse(&bytes),
        Err(QuoteError::TextTooLong {
            field: "dst_address",
            len: 257
        })
    );
}

#[test]
fn quote_hash_matches_golden_vector() {
    let got = fixed_quote().hash().unwrap().to_string();

    // regeneration is opt-in and never green, so a blessing is always a deliberate diff
    if std::env::var("UPDATE_GOLDEN").as_deref() == Ok("1") {
        let dir = std::path::Path::new(GOLDEN).parent().unwrap();
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(GOLDEN, got + "\n").unwrap();
        panic!("golden regenerated, inspect the diff and rerun: {GOLDEN}");
    }

    let want = std::fs::read_to_string(GOLDEN).unwrap_or_else(|e| {
        panic!("golden missing or unreadable ({e}); regenerate with UPDATE_GOLDEN=1: {GOLDEN}")
    });
    assert_eq!(
        got,
        want.trim(),
        "canonical layout changed: breaking, and swapic-backend's mirror breaks with it"
    );
}

/// Pending quotes live in a stable map, so every awkward shape must survive storage.
#[test]
fn every_quote_shape_round_trips_through_storage() {
    use ic_stable_structures::Storable;

    let mut all = vec![fixed_quote()];
    all.extend(one_field_changed().into_iter().map(|(_, q)| q));
    all.extend(edge_quotes());
    for q in all {
        assert_eq!(Quote::from_bytes(q.to_bytes()), q);
    }
}
