#[cfg(test)]
pub(crate) mod tests;

use crate::address::{Address, TextTooLong, TokenId, MAX_TEXT_BYTES};
use crate::canonical::{CanonicalError, CanonicalWriter};
use crate::chain::ChainId;
use crate::evm::{EvmAddress, EvmAddressError};
use crate::hash::QuoteHash;
use crate::numeric::{TokenAmount, UnixSeconds};
use crate::rail::{Rail, UnknownRail};
use ic_stable_structures::storable::{Bound, Storable};
use minicbor::{Decode, Encode};
use sha2::Digest;
use std::borrow::Cow;
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
///
/// Stored as minicbor: `#[n]` indices are append-only, never renumbered or reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(index_only)]
pub enum GasMode {
    #[n(0)]
    Gasless,
    #[n(1)]
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
///
/// Pending quotes are stored as minicbor, a layout apart from the preimage: `#[n]` indices
/// are append-only, never renumbered or reused, and a new field is optional.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub struct Quote {
    #[n(0)]
    pub version: u8,
    #[n(1)]
    pub src_chain: ChainId,
    #[n(2)]
    pub src_token: TokenId,
    #[n(3)]
    pub amount_in: TokenAmount,
    #[n(4)]
    pub dst_chain: ChainId,
    #[n(5)]
    pub dst_token: TokenId,
    #[n(6)]
    pub expected_out: TokenAmount,
    #[n(7)]
    pub min_out: TokenAmount,
    #[n(8)]
    pub dst_address: Address,
    #[n(9)]
    pub refund_address: Option<Address>,
    #[n(10)]
    pub auto_refund: bool,
    #[n(11)]
    pub gas_mode: GasMode,
    #[n(12)]
    pub rail: Rail,
    #[n(13)]
    pub expires_at: UnixSeconds,
    #[n(14)]
    pub nonce: u64,
}

/// Why a quote was refused, naming the field wherever one is at fault.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum QuoteError {
    #[error("version is {0}, and this canister reads layout v{QUOTE_VERSION}")]
    UnsupportedVersion(u8),
    #[error("refund_address is an empty string: leave it absent to mean no refund address")]
    EmptyRefundAddress,
    #[error("{field} is empty")]
    EmptyText { field: &'static str },
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

/// The four fields of a quote that name an account or a token contract. A quote carries
/// them as text, because a rail on another kind of chain names its accounts another way;
/// on an EVM chain each is read as an address through [`Quote::evm_address`], which names
/// the field it read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum QuoteAddressField {
    SrcToken,
    DstToken,
    DstAddress,
    RefundAddress,
}

impl QuoteAddressField {
    /// The field's name as the wire and the preimage table spell it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SrcToken => "src_token",
            Self::DstToken => "dst_token",
            Self::DstAddress => "dst_address",
            Self::RefundAddress => "refund_address",
        }
    }
}

impl std::fmt::Display for QuoteAddressField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a quote's field is not an EVM address, naming the field and the way its text broke.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum QuoteAddressError {
    #[error("the quote's {field} is not an EVM address: {reason}")]
    NotAnAddress {
        field: QuoteAddressField,
        reason: EvmAddressError,
    },
    #[error("the quote names no {field}")]
    Absent { field: QuoteAddressField },
    #[error("the quote's {field} is the zero address, which the vault cannot pay")]
    Zero { field: QuoteAddressField },
}

/// The swap id of a canonical preimage: sha256 over exactly those bytes. Parsing a preimage
/// and encoding it again gives the same bytes back, so a stored preimage is checked against
/// the id it was recorded under without being re-encoded, and without a failure case.
pub fn quote_hash_of(preimage: &[u8]) -> QuoteHash {
    QuoteHash::new(sha2::Sha256::digest(preimage).into())
}

impl Quote {
    /// What a quote must satisfy before the canister holds on to it. A valid quote always
    /// has a canonical preimage.
    pub fn validate(&self) -> Result<(), QuoteError> {
        if self.version != QUOTE_VERSION {
            return Err(QuoteError::UnsupportedVersion(self.version));
        }
        for (field, text) in [
            ("src_token", self.src_token.as_str()),
            ("dst_token", self.dst_token.as_str()),
            ("dst_address", self.dst_address.as_str()),
        ] {
            if text.is_empty() {
                return Err(QuoteError::EmptyText { field });
            }
        }
        for (field, amount) in [
            ("amount_in", self.amount_in),
            ("expected_out", self.expected_out),
            ("min_out", self.min_out),
        ] {
            if amount.try_into_u128().is_none() {
                return Err(QuoteError::AmountTooLarge { field });
            }
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

    /// The text of `field`, or nothing for a refund address the quote does not name.
    fn address_text(&self, field: QuoteAddressField) -> Option<&str> {
        match field {
            QuoteAddressField::SrcToken => Some(self.src_token.as_str()),
            QuoteAddressField::DstToken => Some(self.dst_token.as_str()),
            QuoteAddressField::DstAddress => Some(self.dst_address.as_str()),
            QuoteAddressField::RefundAddress => self.refund_address.as_ref().map(Address::as_str),
        }
    }

    /// The quote's `field` as an EVM address: what every EVM-side reader parses the text
    /// through, so a field that is no address is refused by name with the reason, and no
    /// caller spells the parse for itself. A lower-case and a checksummed spelling read as
    /// the same address.
    pub fn evm_address(&self, field: QuoteAddressField) -> Result<EvmAddress, QuoteAddressError> {
        let text = self
            .address_text(field)
            .ok_or(QuoteAddressError::Absent { field })?;
        text.parse()
            .map_err(|reason| QuoteAddressError::NotAnAddress { field, reason })
    }

    /// The quote's `field` as an address the vault can pay: an EVM address, through
    /// [`Quote::evm_address`], and not the zero address, which the vault's `_send` reverts
    /// on. A payout or a refund to it would revert after the funds had moved, and freeze the
    /// swap for a human, so both doors refuse it. Every chain this canister pays is an EVM
    /// chain, through its vault there, so an EVM address is the one kind a payee can be.
    pub fn payable_address(
        &self,
        field: QuoteAddressField,
    ) -> Result<EvmAddress, QuoteAddressError> {
        let address = self.evm_address(field)?;
        if address == EvmAddress::ZERO {
            return Err(QuoteAddressError::Zero { field });
        }
        Ok(address)
    }

    /// The canonical preimage, or the amount it has no bytes for. Over validated quotes
    /// [`Quote::parse`] is its inverse: `parse(canonical_bytes(q)) == q` for every `q` that
    /// passes [`Quote::validate`]. Outside them it is not: an empty refund address writes
    /// the bytes of an absent one.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, CanonicalError> {
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
        w.finish()
    }

    /// The swap id: sha256 over the canonical preimage and nothing else. A quote with no
    /// preimage has no id.
    pub fn hash(&self) -> Result<QuoteHash, CanonicalError> {
        Ok(quote_hash_of(&self.canonical_bytes()?))
    }

    /// Reads a canonical preimage back. Strict: every byte must be one the writer could
    /// have produced, so parsing then encoding gives back the input. The version is
    /// carried, not interpreted, and empty text is read as it stands; [`Quote::validate`]
    /// refuses what the canister will not hold.
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

crate::storable_as_cbor!(Quote);

/// A quote the store holds, with the height of the quote's source chain the canister held
/// a fresh reading of when it was registered, if it held one.
///
/// The deposit a claim looks for cannot be older than the quote it pays, so that height is
/// where the claim's log read starts, less a margin. Without it the read walks the whole
/// lookback, which is more than a day of blocks on the fastest chain; with it the usual claim reads
/// a few windows. The reading is the watcher's, stamped by the canister on arrival and
/// taken only while it is younger than `chain_data_max_age`, so the height is the
/// watcher's word: it may lag the chain's head, and it may run ahead of it. A claim
/// therefore starts its read the margin below the lower of the height and the provider's
/// own head, and does not stop there: when nothing matches above that, it reads the rest
/// of the lookback before refusing, so no height a watcher pushed strands a deposit.
///
/// Stored as minicbor: `#[n]` indices are append-only, and the height is absent while
/// unknown, so an entry written before it existed reads back without one.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub struct PendingQuote {
    #[n(0)]
    pub quote: Quote,
    #[n(1)]
    pub registered_at: Option<crate::numeric::BlockNumber>,
}

crate::storable_as_cbor!(PendingQuote);

/// A pending quote as the expiry index keys it: by the second it expires, then by swap id,
/// so a walk from the first key meets the soonest expiry first and stops at the first quote
/// still inside its window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExpiryKey {
    pub expires_at: UnixSeconds,
    pub quote_hash: QuoteHash,
}

/// Forty bytes: the eight big-endian bytes of `expires_at`, then the hash, so byte order and
/// key order agree.
impl Storable for ExpiryKey {
    fn to_bytes(&self) -> Cow<'_, [u8]> {
        let mut bytes = Vec::with_capacity(40);
        bytes.extend_from_slice(&self.expires_at.get().to_be_bytes());
        bytes.extend_from_slice(self.quote_hash.as_ref());
        Cow::Owned(bytes)
    }

    fn from_bytes(bytes: Cow<[u8]>) -> Self {
        let (expires_at, quote_hash) = bytes
            .split_first_chunk::<8>()
            .expect("BUG: a stored expiry key is written as exactly 40 bytes");
        Self {
            expires_at: UnixSeconds::new(u64::from_be_bytes(*expires_at)),
            quote_hash: QuoteHash::new(
                quote_hash
                    .try_into()
                    .expect("BUG: a stored expiry key is written as exactly 40 bytes"),
            ),
        }
    }

    const BOUND: Bound = Bound::Bounded {
        max_size: 40,
        is_fixed_size: true,
    };
}
