use super::*;

fn take(w: &mut CanonicalWriter) -> Vec<u8> {
    std::mem::take(w).into_bytes()
}

#[test]
fn integers_are_big_endian_at_their_width() {
    let mut w = CanonicalWriter::default();
    assert_eq!(take(w.put_u8(7)), [7]);
    assert_eq!(take(w.put_u16(0x0102)), [1, 2]);
    assert_eq!(take(w.put_u32(1)), [0, 0, 0, 1]);
    assert_eq!(take(w.put_u64(1)), [0, 0, 0, 0, 0, 0, 0, 1]);
    assert_eq!(take(w.put_bool(true).put_bool(false)), [1, 0]);
}

#[test]
fn an_amount_is_sixteen_bytes_with_the_high_word_kept() {
    let mut w = CanonicalWriter::default();
    let bytes = take(w.put_amount(TokenAmount::from(1u128 << 70)));
    assert_eq!(bytes, (1u128 << 70).to_be_bytes());
}

#[test]
fn text_and_bytes_carry_a_byte_length_even_when_empty() {
    let mut w = CanonicalWriter::default();
    assert_eq!(take(w.put_text("")), [0, 0, 0, 0]);
    // length is bytes, not chars
    assert_eq!(take(w.put_text("\u{2603}")), [0, 0, 0, 3, 0xe2, 0x98, 0x83]);
    assert_eq!(take(w.put_bytes(&[0xde, 0xad])), [0, 0, 0, 2, 0xde, 0xad]);
}

#[test]
fn a_hash_is_raw() {
    let mut w = CanonicalWriter::default();
    assert_eq!(take(w.put_hash(&[9; 32])), [9; 32]);
}
