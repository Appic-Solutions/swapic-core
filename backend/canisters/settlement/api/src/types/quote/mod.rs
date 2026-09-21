use candid::{CandidType, Nat};
use serde::Deserialize;
use types::{ChainId, TokenAmount, UnixSeconds};

/// Who pays the source-side gas. On the wire it is one byte, Gasless 0 and Legacy 1.
#[derive(CandidType, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum GasMode {
    Gasless,
    Legacy,
}

/// What the off-chain quoter handed the user, and the only thing the settlement canister
/// ever hashes into a swap id. The hash is sha256 over a canonical preimage whose field
/// order is frozen and is NOT the order a candid tool prints this record in:
///
/// `version u8 | src_chain u64-be | src_token (u32-be len + utf8) | amount_in u128-be |
/// dst_chain u64 | dst_token | expected_out u128 | min_out u128 | dst_address |
/// refund_address (None encodes as empty) | auto_refund u8 |
/// gas_mode u8 (Gasless=0, Legacy=1) | rail | expires_at_s u64 | nonce u64`
///
/// Every integer is big-endian, every string is a u32-be byte length then utf8, and each
/// of the two one-byte fields is 0 or 1. Amounts must fit in u128, text in 256 bytes, the
/// two tokens and `dst_address` must not be empty, and the rail is one of `cctp_v2_fast`,
/// `cctp_v2_standard`, `eco`. Reproduce those bytes and
/// you reproduce the hash; `backend/libraries/types/golden/quote_hash_v1.txt` is the vector
/// to check a reimplementation against.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq)]
pub struct Quote {
    pub version: u8,
    pub src_chain: u64,
    pub src_token: String,
    pub amount_in: Nat,
    pub dst_chain: u64,
    pub dst_token: String,
    pub expected_out: Nat,
    pub min_out: Nat,
    pub dst_address: String,
    pub refund_address: Option<String>,
    pub auto_refund: bool,
    pub gas_mode: GasMode,
    pub rail: String,
    pub expires_at_s: u64,
    pub nonce: u64,
}

impl From<types::GasMode> for GasMode {
    fn from(mode: types::GasMode) -> Self {
        match mode {
            types::GasMode::Gasless => Self::Gasless,
            types::GasMode::Legacy => Self::Legacy,
        }
    }
}

impl From<GasMode> for types::GasMode {
    fn from(mode: GasMode) -> Self {
        match mode {
            GasMode::Gasless => Self::Gasless,
            GasMode::Legacy => Self::Legacy,
        }
    }
}

impl From<types::Quote> for Quote {
    fn from(quote: types::Quote) -> Self {
        Self {
            version: quote.version,
            src_chain: quote.src_chain.get(),
            src_token: quote.src_token.to_string(),
            amount_in: quote.amount_in.into(),
            dst_chain: quote.dst_chain.get(),
            dst_token: quote.dst_token.to_string(),
            expected_out: quote.expected_out.into(),
            min_out: quote.min_out.into(),
            dst_address: quote.dst_address.to_string(),
            refund_address: quote.refund_address.map(|address| address.to_string()),
            auto_refund: quote.auto_refund,
            gas_mode: quote.gas_mode.into(),
            rail: quote.rail.to_string(),
            expires_at_s: quote.expires_at.get(),
            nonce: quote.nonce,
        }
    }
}

impl TryFrom<Quote> for types::Quote {
    type Error = types::QuoteError;

    fn try_from(quote: Quote) -> Result<Self, Self::Error> {
        use types::QuoteError::{self, AmountTooLarge};
        Ok(Self {
            version: quote.version,
            src_chain: ChainId::new(quote.src_chain),
            src_token: quote
                .src_token
                .parse()
                .map_err(QuoteError::text_too_long("src_token"))?,
            amount_in: TokenAmount::from_canonical_nat(quote.amount_in)
                .ok_or(AmountTooLarge { field: "amount_in" })?,
            dst_chain: ChainId::new(quote.dst_chain),
            dst_token: quote
                .dst_token
                .parse()
                .map_err(QuoteError::text_too_long("dst_token"))?,
            expected_out: TokenAmount::from_canonical_nat(quote.expected_out).ok_or(
                AmountTooLarge {
                    field: "expected_out",
                },
            )?,
            min_out: TokenAmount::from_canonical_nat(quote.min_out)
                .ok_or(AmountTooLarge { field: "min_out" })?,
            dst_address: quote
                .dst_address
                .parse()
                .map_err(QuoteError::text_too_long("dst_address"))?,
            refund_address: quote
                .refund_address
                .map(|address| {
                    address
                        .parse()
                        .map_err(QuoteError::text_too_long("refund_address"))
                })
                .transpose()?,
            auto_refund: quote.auto_refund,
            gas_mode: quote.gas_mode.into(),
            rail: quote.rail.parse()?,
            expires_at: UnixSeconds::new(quote.expires_at_s),
            nonce: quote.nonce,
        })
    }
}

/// Why a quote was refused, naming the field wherever one is at fault.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum QuoteError {
    UnsupportedVersion(u8),
    EmptyRefundAddress,
    EmptyText {
        field: String,
    },
    AmountTooLarge {
        field: String,
    },
    TextTooLong {
        field: String,
        len: u64,
    },
    UnknownRail(String),
    Truncated {
        field: String,
        at: u64,
        wanted: u64,
        len: u64,
    },
    NotABool {
        field: String,
        value: u8,
    },
    NotAGasMode(u8),
    NotUtf8 {
        field: String,
    },
    TrailingBytes {
        consumed: u64,
        len: u64,
    },
}

impl From<types::QuoteError> for QuoteError {
    fn from(error: types::QuoteError) -> Self {
        use types::QuoteError as Domain;
        match error {
            Domain::UnsupportedVersion(version) => Self::UnsupportedVersion(version),
            Domain::EmptyRefundAddress => Self::EmptyRefundAddress,
            Domain::EmptyText { field } => Self::EmptyText {
                field: field.to_string(),
            },
            Domain::AmountTooLarge { field } => Self::AmountTooLarge {
                field: field.to_string(),
            },
            Domain::TextTooLong { field, len } => Self::TextTooLong {
                field: field.to_string(),
                len: crate::types::wire_len(len),
            },
            Domain::UnknownRail(types::rail::UnknownRail(rail)) => Self::UnknownRail(rail),
            Domain::Truncated {
                field,
                at,
                wanted,
                len,
            } => Self::Truncated {
                field: field.to_string(),
                at: crate::types::wire_len(at),
                wanted: crate::types::wire_len(wanted),
                len: crate::types::wire_len(len),
            },
            Domain::NotABool { field, value } => Self::NotABool {
                field: field.to_string(),
                value,
            },
            Domain::NotAGasMode(mode) => Self::NotAGasMode(mode),
            Domain::NotUtf8 { field } => Self::NotUtf8 {
                field: field.to_string(),
            },
            Domain::TrailingBytes { consumed, len } => Self::TrailingBytes {
                consumed: crate::types::wire_len(consumed),
                len: crate::types::wire_len(len),
            },
        }
    }
}

#[cfg(test)]
mod tests;
