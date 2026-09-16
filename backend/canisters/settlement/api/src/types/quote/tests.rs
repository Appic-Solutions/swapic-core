use super::*;

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
        expires_at_s: _,
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
                src_chain: 1,
                ..q.clone()
            },
        ),
        (
            "src_token",
            Quote {
                src_token: "0xdAC17F958D2ee523a2206206994597C13D831ec7".into(),
                ..q.clone()
            },
        ),
        (
            "amount_in",
            Quote {
                amount_in: 25_000_001,
                ..q.clone()
            },
        ),
        (
            "dst_chain",
            Quote {
                dst_chain: 1,
                ..q.clone()
            },
        ),
        (
            "dst_token",
            Quote {
                dst_token: "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48".into(),
                ..q.clone()
            },
        ),
        (
            "expected_out",
            Quote {
                expected_out: 24_990_001,
                ..q.clone()
            },
        ),
        (
            "min_out",
            Quote {
                min_out: 24_900_001,
                ..q.clone()
            },
        ),
        (
            "dst_address",
            Quote {
                dst_address: "0x1111111254EEB25477B68fb85Ed929f73A960582".into(),
                ..q.clone()
            },
        ),
        (
            // a non-empty address, deliberately: None and Some("") share a preimage
            // by design, which `none_and_empty_refund_address_share_a_preimage` pins
            "refund_address",
            Quote {
                refund_address: Some("0x1111111254EEB25477B68fb85Ed929f73A960582".into()),
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
                rail: "cctp_v2_standard".into(),
                ..q.clone()
            },
        ),
        (
            "expires_at_s",
            Quote {
                expires_at_s: 1_800_000_001,
                ..q.clone()
            },
        ),
        ("nonce", Quote { nonce: 8, ..q }),
    ]
}

/// The awkward shapes: empty strings, multibyte utf8, and the top of every integer.
fn edge_quotes() -> Vec<Quote> {
    vec![
        Quote {
            src_token: String::new(),
            dst_token: String::new(),
            dst_address: String::new(),
            rail: String::new(),
            refund_address: None,
            ..fixed_quote()
        },
        Quote {
            // length is bytes, not chars
            src_token: "tökén \u{2603}".into(),
            dst_address: "\u{1F680}".into(),
            refund_address: Some("réfund".into()),
            rail: "rail \u{2603}".into(),
            ..fixed_quote()
        },
        Quote {
            version: u8::MAX,
            src_chain: u64::MAX,
            // high words set: proves the u128s are 16 bytes, not truncated u64s
            amount_in: u128::MAX,
            dst_chain: u64::MAX,
            expected_out: u128::MAX - 1,
            min_out: 1u128 << 70,
            auto_refund: false,
            gas_mode: GasMode::Gasless,
            expires_at_s: u64::MAX,
            nonce: u64::MAX,
            ..fixed_quote()
        },
    ]
}

const GOLDEN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/golden/quote_hash_v1.txt");

#[test]
fn any_field_change_changes_the_hash() {
    let base = quote_hash(&fixed_quote());
    let changed = one_field_changed();
    assert_eq!(
        changed.len(),
        QUOTE_FIELD_COUNT,
        "one_field_changed() must move every field"
    );
    let mut seen = std::collections::BTreeSet::new();
    for (field, q) in changed {
        let h = quote_hash(&q);
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
    // + 8 expires_at_s + 8 nonce
    assert_eq!(quote_bytes(&fixed_quote()).len(), 241);
}

#[test]
fn gas_mode_encodes_one_byte() {
    let gasless = quote_bytes(&Quote {
        gas_mode: GasMode::Gasless,
        ..fixed_quote()
    });
    let legacy = quote_bytes(&fixed_quote());
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
        refund_address: Some(String::new()),
        ..fixed_quote()
    };
    assert_eq!(quote_bytes(&none), quote_bytes(&empty));
    none.validate().expect("an absent refund address is fine");
    let err = empty.validate().expect_err("an empty one is not");
    assert!(err.contains("refund_address"), "name the field: {err}");
    // and the reader resolves it the one way that survives a round trip
    assert_eq!(parse_quote(&quote_bytes(&none)).unwrap(), none);
}

#[test]
fn validate_refuses_a_version_this_canister_cannot_read() {
    let err = Quote {
        version: 2,
        ..fixed_quote()
    }
    .validate()
    .expect_err("only v1 is known");
    assert!(err.contains("version"), "name the field: {err}");
}

#[test]
fn parse_quote_round_trips_every_field_variation() {
    let mut all = vec![fixed_quote()];
    all.extend(one_field_changed().into_iter().map(|(_, q)| q));
    all.extend(edge_quotes());
    for q in all {
        let bytes = quote_bytes(&q);
        let back = parse_quote(&bytes).unwrap_or_else(|e| panic!("{e} for {q:?}"));
        assert_eq!(back, q, "round trip lost a field");
        assert_eq!(quote_bytes(&back), bytes, "and the re-encode drifted");
    }
}

#[test]
fn parse_quote_rejects_a_truncated_preimage() {
    let bytes = quote_bytes(&fixed_quote());
    for cut in [0, 1, 40, 100, bytes.len() - 1] {
        let err = parse_quote(&bytes[..cut]).expect_err("truncated bytes are not a quote");
        assert!(err.contains("truncated"), "say what went wrong: {err}");
    }
}

#[test]
fn parse_quote_rejects_trailing_bytes() {
    let mut bytes = quote_bytes(&fixed_quote());
    bytes.push(0);
    let err = parse_quote(&bytes).expect_err("a quote is the whole input");
    assert!(err.contains("trailing"), "say what went wrong: {err}");
}

#[test]
fn parse_quote_rejects_bytes_outside_the_layout() {
    let base = quote_bytes(&fixed_quote());
    let auto_refund_at = base.len() - 8 - 8 - (4 + 12) - 1 - 1;

    let mut bad_bool = base.clone();
    bad_bool[auto_refund_at] = 2;
    let err = parse_quote(&bad_bool).expect_err("a bool is 0 or 1");
    assert!(err.contains("auto_refund"), "name the field: {err}");

    let mut bad_mode = base.clone();
    bad_mode[auto_refund_at + 1] = 2;
    let err = parse_quote(&bad_mode).expect_err("a gas mode is 0 or 1");
    assert!(err.contains("gas_mode"), "name the field: {err}");

    // the src_token length prefix, blown up past the end of the input
    let mut long_str = base.clone();
    long_str[9..13].copy_from_slice(&u32::MAX.to_be_bytes());
    assert!(parse_quote(&long_str).is_err(), "a length must fit");

    // 0xff is never valid utf8, so this lands inside src_token's bytes
    let mut bad_utf8 = base;
    bad_utf8[13] = 0xff;
    let err = parse_quote(&bad_utf8).expect_err("strings are utf8");
    assert!(err.contains("src_token"), "name the field: {err}");
}

#[test]
fn quote_hash_matches_golden_vector() {
    let got = hex::encode(quote_hash(&fixed_quote()));

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
