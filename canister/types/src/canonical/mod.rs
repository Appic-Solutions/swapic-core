//! The primitives both canonical preimages are made of. Integers are big-endian, an amount
//! is 16 bytes, a hash is its 32 raw bytes, and a byte string or a text is a u32
//! big-endian length followed by the bytes. The layouts themselves live with their types:
//! the quote's in `quote`, the event's in `events`.

#[cfg(test)]
mod tests;

use crate::checked_amount::CheckedAmountOf;
use crate::numeric::TokenAmount;
use thiserror::Error;

/// A value the canonical layout has no bytes for.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum CanonicalError {
    #[error("amount {0} is above u128::MAX, the most a 16-byte amount field holds")]
    AmountTooLarge(TokenAmount),
    #[error("{len} bytes do not fit a u32 length prefix")]
    TooLong { len: usize },
}

/// Builds a canonical preimage, one typed field at a time. A field with no encoding is
/// kept as the first error and [`CanonicalWriter::finish`] returns it, so a layout reads
/// as one chain of puts and never panics.
#[derive(Default)]
pub struct CanonicalWriter {
    bytes: Vec<u8>,
    error: Option<CanonicalError>,
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

    /// Sixteen bytes, whatever the amount counts. Above `u128::MAX` there are none, and
    /// `finish` fails.
    pub fn put_amount<Unit>(&mut self, amount: CheckedAmountOf<Unit>) -> &mut Self {
        match amount.try_into_u128() {
            Some(value) => self.bytes.extend_from_slice(&value.to_be_bytes()),
            None => self.fail(CanonicalError::AmountTooLarge(amount.change_units())),
        }
        self
    }

    /// The 32 raw bytes, without a length.
    pub fn put_hash(&mut self, hash: &[u8; 32]) -> &mut Self {
        self.bytes.extend_from_slice(hash);
        self
    }

    /// The 20 raw bytes of an EVM address, without a length.
    pub fn put_address(&mut self, address: &[u8; 20]) -> &mut Self {
        self.bytes.extend_from_slice(address);
        self
    }

    /// A u32 length, then the bytes. At 4 GiB and over there is no length, and `finish`
    /// fails.
    pub fn put_bytes(&mut self, bytes: &[u8]) -> &mut Self {
        match u32::try_from(bytes.len()) {
            Ok(len) => {
                self.put_u32(len);
                self.bytes.extend_from_slice(bytes);
            }
            Err(_) => self.fail(CanonicalError::TooLong { len: bytes.len() }),
        }
        self
    }

    /// A u32 length, then the utf8 bytes.
    pub fn put_text(&mut self, text: &str) -> &mut Self {
        self.put_bytes(text.as_bytes())
    }

    /// The preimage, or the first field that had no encoding.
    pub fn finish(self) -> Result<Vec<u8>, CanonicalError> {
        match self.error {
            Some(error) => Err(error),
            None => Ok(self.bytes),
        }
    }

    fn fail(&mut self, error: CanonicalError) {
        self.error.get_or_insert(error);
    }
}
