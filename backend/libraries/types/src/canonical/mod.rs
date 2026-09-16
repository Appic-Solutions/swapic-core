//! The primitives both canonical preimages are made of. Integers are big-endian, an amount
//! is 16 bytes, a hash is its 32 raw bytes, and a byte string or a text is a u32
//! big-endian length followed by the bytes. The layouts themselves live with their types:
//! the quote's in `quote`, the event's in `events`.

#[cfg(test)]
mod tests;

use crate::numeric::TokenAmount;

/// Builds a canonical preimage, one typed field at a time.
#[derive(Default)]
pub struct CanonicalWriter {
    bytes: Vec<u8>,
}

impl CanonicalWriter {
    /// One byte.
    pub fn put_u8(&mut self, value: u8) -> &mut Self {
        self.bytes.push(value);
        self
    }

    /// One byte, 0 or 1.
    pub fn put_bool(&mut self, value: bool) -> &mut Self {
        self.put_u8(u8::from(value))
    }

    /// Two bytes.
    pub fn put_u16(&mut self, value: u16) -> &mut Self {
        self.bytes.extend_from_slice(&value.to_be_bytes());
        self
    }

    /// Four bytes.
    pub fn put_u32(&mut self, value: u32) -> &mut Self {
        self.bytes.extend_from_slice(&value.to_be_bytes());
        self
    }

    /// Eight bytes.
    pub fn put_u64(&mut self, value: u64) -> &mut Self {
        self.bytes.extend_from_slice(&value.to_be_bytes());
        self
    }

    /// Sixteen bytes.
    pub fn put_amount(&mut self, amount: TokenAmount) -> &mut Self {
        let amount = amount.try_into_u128().expect(
            "BUG: every conversion into a canonical amount field rejects values above u128::MAX",
        );
        self.bytes.extend_from_slice(&amount.to_be_bytes());
        self
    }

    /// The 32 raw bytes, without a length.
    pub fn put_hash(&mut self, hash: &[u8; 32]) -> &mut Self {
        self.bytes.extend_from_slice(hash);
        self
    }

    /// A u32 length, then the bytes.
    pub fn put_bytes(&mut self, bytes: &[u8]) -> &mut Self {
        let len = u32::try_from(bytes.len()).expect(
            "BUG: no canonical field reaches 4 GiB, the canister's own message limit is 2 MiB",
        );
        self.put_u32(len);
        self.bytes.extend_from_slice(bytes);
        self
    }

    /// A u32 length, then the utf8 bytes.
    pub fn put_text(&mut self, text: &str) -> &mut Self {
        self.put_bytes(text.as_bytes())
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}
