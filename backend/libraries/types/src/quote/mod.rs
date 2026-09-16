#[cfg(test)]
mod tests;

use crate::address::{Address, TextTooLong, TokenId, MAX_TEXT_BYTES};
use crate::canonical::CanonicalWriter;
use crate::chain::ChainId;
use crate::hash::QuoteHash;
use crate::numeric::{TokenAmount, UnixSeconds};
use crate::rail::{Rail, UnknownRail};
use sha2::Digest;
use std::time::Duration;
use thiserror::Error;

/// The only layout this canister speaks. The version byte is the first byte of the
/// preimage, so a future layout is a new parser and a new golden file, never a branch
/// inside this one.
pub const QUOTE_VERSION: u8 = 1;

/// The number of fields in a [`Quote`]. The exhaustive destructure in
/// [`Quote::canonical_bytes`] is the compile-time check, the tests are the coverage check.
pub const QUOTE_FIELD_COUNT: usize = 15;

/// The furthest ahead a quote may expire, so a quote cannot hold a pending slot forever.
pub const MAX_QUOTE_LIFETIME: Duration = Duration::from_secs(86_400);

/// Who pays the source-side gas. One byte in the preimage: Gasless 0, Legacy 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GasMode {
    Gasless,
    Legacy,
}

/// What the off-chain quoter handed the user, and the only thing the canister hashes into
/// a swap id. The hash is sha256 over this canonical preimage, a cross-repo contract that
/// swapic-backend's quoter reproduces byte for byte:
///
/// | field            | encoding                                  |
/// |------------------|-------------------------------------------|
/// | `version`        | u8                                        |
/// | `src_chain`      | u64                                       |
/// | `src_token`      | text                                      |
/// | `amount_in`      | u128                                      |
/// | `dst_chain`      | u64                                       |
/// | `dst_token`      | text                                      |
/// | `expected_out`   | u128                                      |
/// | `min_out`        | u128                                      |
/// | `dst_address`    | text                                      |
/// | `refund_address` | text, the empty text when absent          |
/// | `auto_refund`    | u8, 0 or 1                                |
/// | `gas_mode`       | u8, Gasless 0 and Legacy 1                |
/// | `rail`           | text, the rail id                         |
/// | `expires_at`     | u64 seconds                               |
/// | `nonce`          | u64                                       |
///
/// Integers are big-endian and text is a u32 big-endian byte length then utf8. The order
/// is frozen. `backend/libraries/types/golden/quote_hash_v1.txt` is the vector to check a
/// reimplementation against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Quote {
    pub version: u8,
    pub src_chain: ChainId,
    pub src_token: TokenId,
    pub amount_in: TokenAmount,
    pub dst_chain: ChainId,
    pub dst_token: TokenId,
    pub expected_out: TokenAmount,
    pub min_out: TokenAmount,
    pub dst_address: Address,
    pub refund_address: Option<Address>,
    pub auto_refund: bool,
    pub gas_mode: GasMode,
    pub rail: Rail,
    pub expires_at: UnixSeconds,
    pub nonce: u64,
}

/// Why a quote was refused, naming the field wherever one is at fault.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum QuoteError {
    #[error("version is {0}, and this canister reads layout v{QUOTE_VERSION}")]
    UnsupportedVersion(u8),
    #[error("refund_address is an empty string: leave it absent to mean no refund address")]
    EmptyRefundAddress,
    #[error("{field} is above u128::MAX")]
    AmountTooLarge { field: &'static str },
    #[error("{field} is {len} bytes, above the cap of {MAX_TEXT_BYTES}")]
    TextTooLong { field: &'static str, len: usize },
    #[error("rail: {0}")]
    UnknownRail(#[from] UnknownRail),
    #[error("{field}: truncated at byte {at}, wanted {wanted} more of {len}")]
    Truncated {
        field: &'static str,
        at: usize,
        wanted: usize,
        len: usize,
    },
    #[error("{field}: {value} is not a bool")]
    NotABool { field: &'static str, value: u8 },
    #[error("gas_mode: {0} is not a gas mode")]
    NotAGasMode(u8),
    #[error("{field}: not utf8")]
    NotUtf8 { field: &'static str },
    #[error("trailing bytes: {consumed} of {len} consumed")]
    TrailingBytes { consumed: usize, len: usize },
}

impl QuoteError {
    /// Names `field` in a text length failure.
    pub fn text_too_long(field: &'static str) -> impl FnOnce(TextTooLong) -> Self {
        move |TextTooLong { len }| Self::TextTooLong { field, len }
    }
}

impl Quote {
    /// What a quote must satisfy before the canister holds on to it.
    pub fn validate(&self) -> Result<(), QuoteError> {
        if self.version != QUOTE_VERSION {
            return Err(QuoteError::UnsupportedVersion(self.version));
        }
        // an empty refund address writes the same bytes as an absent one, so only the
        // absent one is accepted
        if self
            .refund_address
            .as_ref()
            .is_some_and(|address| address.as_str().is_empty())
        {
            return Err(QuoteError::EmptyRefundAddress);
        }
        Ok(())
    }

    /// The canonical preimage. [`Quote::parse`] is its exact inverse.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        // exhaustive: a new field fails to compile until the layout decides where it goes
        let Quote {
            version,
            src_chain,
            src_token,
            amount_in,
            dst_chain,
            dst_token,
            expected_out,
            min_out,
            dst_address,
            refund_address,
            auto_refund,
            gas_mode,
            rail,
            expires_at,
            nonce,
        } = self;
        let mut w = CanonicalWriter::default();
        w.put_u8(*version)
            .put_u64(src_chain.get())
            .put_text(src_token.as_str())
            .put_amount(*amount_in)
            .put_u64(dst_chain.get())
            .put_text(dst_token.as_str())
            .put_amount(*expected_out)
            .put_amount(*min_out)
            .put_text(dst_address.as_str())
            .put_text(refund_address.as_ref().map_or("", Address::as_str))
            .put_bool(*auto_refund)
            .put_u8(match gas_mode {
                GasMode::Gasless => 0,
                GasMode::Legacy => 1,
            })
            .put_text(rail.as_str())
            .put_u64(expires_at.get())
            .put_u64(*nonce);
        w.into_bytes()
    }

    /// The swap id: sha256 over the canonical preimage and nothing else.
    pub fn hash(&self) -> QuoteHash {
        QuoteHash::new(sha2::Sha256::digest(self.canonical_bytes()).into())
    }

    /// Reads a canonical preimage back. Strict: every byte must be one the writer could
    /// have produced, so parsing then encoding gives back the input. The version is
    /// carried, not interpreted; [`Quote::validate`] refuses one the canister cannot read.
    pub fn parse(bytes: &[u8]) -> Result<Quote, QuoteError> {
        let mut r = Reader { bytes, at: 0 };
        let quote = Quote {
            version: r.u8("version")?,
            src_chain: ChainId::new(r.u64("src_chain")?),
            src_token: r.text("src_token")?,
            amount_in: TokenAmount::from(r.u128("amount_in")?),
            dst_chain: ChainId::new(r.u64("dst_chain")?),
            dst_token: r.text("dst_token")?,
            expected_out: TokenAmount::from(r.u128("expected_out")?),
            min_out: TokenAmount::from(r.u128("min_out")?),
            dst_address: r.text("dst_address")?,
            refund_address: Some(r.text::<Address>("refund_address")?)
                .filter(|address| !address.as_str().is_empty()),
            auto_refund: r.bool("auto_refund")?,
            gas_mode: match r.u8("gas_mode")? {
                0 => GasMode::Gasless,
                1 => GasMode::Legacy,
                other => return Err(QuoteError::NotAGasMode(other)),
            },
            rail: r.string("rail")?.parse()?,
            expires_at: UnixSeconds::new(r.u64("expires_at")?),
            nonce: r.u64("nonce")?,
        };
        r.finish()?;
        Ok(quote)
    }
}

/// Reads the canonical primitives in order. Struct fields are evaluated in the order they
/// are written, so [`Quote::parse`] reads in layout order.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take<const N: usize>(&mut self, field: &'static str) -> Result<[u8; N], QuoteError> {
        let bytes = self.take_slice(N, field)?;
        Ok(bytes
            .try_into()
            .expect("BUG: take_slice returns exactly N bytes"))
    }

    fn take_slice(&mut self, wanted: usize, field: &'static str) -> Result<&'a [u8], QuoteError> {
        let truncated = QuoteError::Truncated {
            field,
            at: self.at,
            wanted,
            len: self.bytes.len(),
        };
        let end = self.at.checked_add(wanted).ok_or(truncated.clone())?;
        let out = self.bytes.get(self.at..end).ok_or(truncated)?;
        self.at = end;
        Ok(out)
    }

    fn u8(&mut self, field: &'static str) -> Result<u8, QuoteError> {
        self.take::<1>(field).map(u8::from_be_bytes)
    }

    fn u64(&mut self, field: &'static str) -> Result<u64, QuoteError> {
        self.take::<8>(field).map(u64::from_be_bytes)
    }

    fn u128(&mut self, field: &'static str) -> Result<u128, QuoteError> {
        self.take::<16>(field).map(u128::from_be_bytes)
    }

    fn bool(&mut self, field: &'static str) -> Result<bool, QuoteError> {
        match self.u8(field)? {
            0 => Ok(false),
            1 => Ok(true),
            value => Err(QuoteError::NotABool { field, value }),
        }
    }

    fn string(&mut self, field: &'static str) -> Result<String, QuoteError> {
        let len = u32::from_be_bytes(self.take::<4>(field)?);
        let wanted = usize::try_from(len).expect("BUG: the canister targets 32 and 64 bit usize");
        let bytes = self.take_slice(wanted, field)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| QuoteError::NotUtf8 { field })
    }

    fn text<T: std::str::FromStr<Err = TextTooLong>>(
        &mut self,
        field: &'static str,
    ) -> Result<T, QuoteError> {
        self.string(field)?
            .parse()
            .map_err(QuoteError::text_too_long(field))
    }

    fn finish(self) -> Result<(), QuoteError> {
        if self.at == self.bytes.len() {
            Ok(())
        } else {
            Err(QuoteError::TrailingBytes {
                consumed: self.at,
                len: self.bytes.len(),
            })
        }
    }
}
