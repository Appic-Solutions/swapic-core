use crate::types::events::EvmAddressError;
use candid::CandidType;
use serde::Deserialize;

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

/// The four fields of a quote that name an account or a token contract.
#[derive(CandidType, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuoteAddressField {
    SrcToken,
    DstToken,
    DstAddress,
    RefundAddress,
}

impl From<types::quote::QuoteAddressField> for QuoteAddressField {
    fn from(field: types::quote::QuoteAddressField) -> Self {
        use types::quote::QuoteAddressField as Domain;
        match field {
            Domain::SrcToken => Self::SrcToken,
            Domain::DstToken => Self::DstToken,
            Domain::DstAddress => Self::DstAddress,
            Domain::RefundAddress => Self::RefundAddress,
        }
    }
}

/// Why a quote's field is not an EVM address: the field, and the way its text broke, or
/// that the quote does not name it.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum QuoteAddressError {
    NotAnAddress {
        field: QuoteAddressField,
        reason: EvmAddressError,
    },
    Absent {
        field: QuoteAddressField,
    },
    /// The field is the zero address, which the vault's `_send` reverts on: a payout or a
    /// refund to it would freeze the swap after the funds had moved.
    Zero {
        field: QuoteAddressField,
    },
}

impl From<types::quote::QuoteAddressError> for QuoteAddressError {
    fn from(error: types::quote::QuoteAddressError) -> Self {
        use types::quote::QuoteAddressError as Domain;
        match error {
            Domain::NotAnAddress { field, reason } => Self::NotAnAddress {
                field: field.into(),
                reason: reason.into(),
            },
            Domain::Absent { field } => Self::Absent {
                field: field.into(),
            },
            Domain::Zero { field } => Self::Zero {
                field: field.into(),
            },
        }
    }
}

/// Why a quote is not one its rail can carry: the rails carry the configured USDC on both
/// sides and nothing else, compared as addresses. Addresses are EIP-55, the rail is its
/// id.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum RailTokenError {
    NotTheRailToken {
        field: QuoteAddressField,
        quoted: String,
        rail_token: String,
        rail: String,
    },
    QuoteAddress(QuoteAddressError),
    /// The chain has no token configured for the rail.
    NoRailToken {
        chain_id: u64,
        rail: String,
    },
}

impl From<types::rail::RailTokenError> for RailTokenError {
    fn from(error: types::rail::RailTokenError) -> Self {
        use types::rail::RailTokenError as Domain;
        match error {
            Domain::NotTheRailToken {
                field,
                quoted,
                rail_token,
                rail,
            } => Self::NotTheRailToken {
                field: field.into(),
                quoted: quoted.to_string(),
                rail_token: rail_token.to_string(),
                rail: rail.to_string(),
            },
            Domain::QuoteAddress(error) => Self::QuoteAddress(error.into()),
            Domain::NoRailToken { chain_id, rail } => Self::NoRailToken {
                chain_id: chain_id.get(),
                rail: rail.to_string(),
            },
        }
    }
}

#[cfg(test)]
mod tests;
