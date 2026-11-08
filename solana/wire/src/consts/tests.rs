use super::*;

#[test]
fn base58_matches_known_bytes() {
    // The first bytes of the two addresses the research recorded as raw arrays.
    assert_eq!(&TOKEN_PROGRAM[..4], &[6, 221, 246, 225]);
    assert_eq!(&USDC_MINT[..4], &[198, 250, 122, 243]);
    assert_eq!(&WSOL_MINT[..4], &[6, 155, 136, 87]);
    assert_eq!(b58("11111111111111111111111111111111"), [0u8; 32]);
}

#[test]
fn shard_is_the_low_bits_of_the_last_byte() {
    let mut r = [0xffu8; 32];
    r[31] = 0b1010_0110;
    assert_eq!(shard_of(&r), 2);
    r[31] = 3;
    assert_eq!(shard_of(&r), 3);
}
