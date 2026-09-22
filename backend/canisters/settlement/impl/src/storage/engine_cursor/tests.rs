use super::*;
use crate::storage::on_fresh_memory;

/// The cell opens empty, keeps what a tick puts there, and reads an unexpected value as
/// empty rather than trapping the pass that reads it.
#[test]
fn the_cursor_opens_empty_and_keeps_the_last_swap_a_tick_drove() {
    on_fresh_memory(|| {
        crate::storage::init();
        assert_eq!(get(), None, "a fresh install starts at the beginning");
        let quote_hash = QuoteHash::new([0x5a; 32]);
        set(quote_hash);
        assert_eq!(get(), Some(quote_hash));
        set(QuoteHash::new([0x5b; 32]));
        assert_eq!(get(), Some(QuoteHash::new([0x5b; 32])));
    });
    assert_eq!(Cursor::from_bytes(Cow::Borrowed(&[])), Cursor(None));
    assert_eq!(Cursor::from_bytes(Cow::Borrowed(&[1, 2, 3])), Cursor(None));
    assert_eq!(
        Cursor(Some(QuoteHash::new([0x5a; 32]))).to_bytes().as_ref(),
        &[0x5a; 32],
    );
}
