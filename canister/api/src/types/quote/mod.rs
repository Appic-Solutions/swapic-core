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

#[cfg(test)]
mod tests;
