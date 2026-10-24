use super::*;
use types::address::MAX_TEXT_BYTES;
use types::rail::UnknownRail;

#[test]
fn a_quote_error_names_its_field_on_the_wire() {
    assert_eq!(
        QuoteError::from(types::QuoteError::TextTooLong {
            field: "dst_address",
            len: MAX_TEXT_BYTES + 1,
        }),
        QuoteError::TextTooLong {
            field: "dst_address".into(),
            len: MAX_TEXT_BYTES as u64 + 1
        }
    );
    assert_eq!(
        QuoteError::from(types::QuoteError::UnknownRail(UnknownRail("cctp".into()))),
        QuoteError::UnknownRail("cctp".into())
    );
    assert_eq!(
        QuoteError::from(types::QuoteError::EmptyText { field: "dst_token" }),
        QuoteError::EmptyText {
            field: "dst_token".into()
        }
    );
}
