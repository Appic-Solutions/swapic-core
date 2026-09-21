pub mod config;
pub mod errors;
pub mod events;
pub mod init;
pub mod quote;
pub mod swap;

use candid::Nat;
use std::str::FromStr;
use types::address::TextTooLong;
use types::TokenAmount;

/// A length or position as the wire carries it.
pub(crate) fn wire_len(len: usize) -> u64 {
    u64::try_from(len).expect("BUG: usize is at most 64 bits on every target")
}

/// Wire text as one of the bounded text types, or the raw failure: its length. The caller
/// names the field in its own error.
pub(crate) fn text<T: FromStr<Err = TextTooLong>>(value: &str) -> Result<T, TextTooLong> {
    value.parse()
}

/// A wire amount as an amount a canonical field holds, or the raw failure: `None` above
/// `u128::MAX`. The caller names the field in its own error.
pub(crate) fn amount(value: Nat) -> Option<TokenAmount> {
    TokenAmount::from_canonical_nat(value)
}
