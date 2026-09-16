use super::*;

#[test]
fn display_and_debug_print_lowercase_hex() {
    let mut bytes = [0u8; 32];
    bytes[0] = 0xab;
    bytes[31] = 0x0f;
    let hash = QuoteHash::new(bytes);
    let hex = format!("ab{}0f", "00".repeat(30));
    assert_eq!(hash.to_string(), hex);
    assert_eq!(format!("{hash:?}"), hex);
}

#[test]
fn the_genesis_parent_is_all_zeros() {
    assert_eq!(EventHash::ZERO.into_bytes(), [0; 32]);
}

#[test]
fn cbor_writes_a_byte_string() {
    let hash = TxHash::new([7; 32]);
    let bytes = minicbor::to_vec(hash).unwrap();
    // major type 2 with a one-byte length, then the 32 bytes
    assert_eq!(&bytes[..2], &[0x58, 32]);
    assert_eq!(minicbor::decode::<TxHash>(&bytes).unwrap(), hash);
}
