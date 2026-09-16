// The length-prefixed primitives of the event preimage: a u32-be length then the bytes.
fn put_len(b: &mut Vec<u8>, len: usize) {
    let n = u32::try_from(len).expect("field length fits u32");
    b.extend_from_slice(&n.to_be_bytes());
}

pub(crate) fn put_bytes(b: &mut Vec<u8>, v: &[u8]) {
    put_len(b, v.len());
    b.extend_from_slice(v);
}

pub(crate) fn put_str(b: &mut Vec<u8>, s: &str) {
    put_bytes(b, s.as_bytes());
}
