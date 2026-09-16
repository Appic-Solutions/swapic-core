use super::*;
use types::address::MAX_TEXT_BYTES;
use types::rail::UnknownRail;

/// The cross-repo fixture as the quoter sends it.
fn fixed_quote() -> Quote {
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

const GOLDEN: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../libraries/types/golden/quote_hash_v1.txt"
);

/// Conversion keeps every byte of text as it arrived, so the wire fixture hashes to the
/// golden line the quoter computes on its side.
#[test]
fn the_wire_fixture_hashes_to_the_golden_vector() {
    let quote = types::Quote::try_from(fixed_quote()).unwrap();
    let want = std::fs::read_to_string(GOLDEN).expect("golden vector, committed");
    assert_eq!(quote.hash().to_string(), want.trim());
}

#[test]
fn a_quote_survives_the_wire_both_ways() {
    let edge = Quote {
        src_token: String::new(),
        dst_address: "\u{1F680}".into(),
        refund_address: Some("réfund".into()),
        amount_in: Nat::from(u128::MAX),
        min_out: Nat::from(1u128 << 70),
        gas_mode: GasMode::Gasless,
        rail: "eco".into(),
        expires_at_s: u64::MAX,
        ..fixed_quote()
    };
    for wire in [fixed_quote(), edge] {
        let domain = types::Quote::try_from(wire.clone()).unwrap();
        assert_eq!(Quote::from(domain), wire);
    }
}

#[test]
fn an_amount_above_u128_max_is_refused_naming_the_field() {
    let too_large = Nat::from(u128::MAX) + Nat::from(1_u8);
    type SetField = fn(&mut Quote, Nat);
    let fields: [(&str, SetField); 3] = [
        ("amount_in", |q, n| q.amount_in = n),
        ("expected_out", |q, n| q.expected_out = n),
        ("min_out", |q, n| q.min_out = n),
    ];
    for (field, set) in fields {
        let mut quote = fixed_quote();
        set(&mut quote, too_large.clone());
        assert_eq!(
            types::Quote::try_from(quote),
            Err(QuoteError::AmountTooLarge { field })
        );
        let mut at_max = fixed_quote();
        set(&mut at_max, Nat::from(u128::MAX));
        assert!(
            types::Quote::try_from(at_max).is_ok(),
            "{field} at u128::MAX"
        );
    }
}

#[test]
fn text_over_the_cap_is_refused_naming_the_field() {
    type SetField = fn(&mut Quote, String);
    let fields: [(&str, SetField); 4] = [
        ("src_token", |q, s| q.src_token = s),
        ("dst_token", |q, s| q.dst_token = s),
        ("dst_address", |q, s| q.dst_address = s),
        ("refund_address", |q, s| q.refund_address = Some(s)),
    ];
    for (field, set) in fields {
        let mut over = fixed_quote();
        set(&mut over, "a".repeat(MAX_TEXT_BYTES + 1));
        assert_eq!(
            types::Quote::try_from(over),
            Err(QuoteError::TextTooLong {
                field,
                len: MAX_TEXT_BYTES + 1
            })
        );
    }
}

#[test]
fn a_rail_must_be_one_of_the_known_ids() {
    for rail in ["cctp_v2_fast", "cctp_v2_standard", "eco"] {
        let quote = Quote {
            rail: rail.into(),
            ..fixed_quote()
        };
        assert!(types::Quote::try_from(quote).is_ok(), "{rail}");
    }
    let unknown = Quote {
        rail: "CCTP_V2_FAST".into(),
        ..fixed_quote()
    };
    assert_eq!(
        types::Quote::try_from(unknown),
        Err(QuoteError::UnknownRail(UnknownRail("CCTP_V2_FAST".into())))
    );
}
