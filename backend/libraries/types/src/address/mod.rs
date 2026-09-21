#[cfg(test)]
mod tests;

use minicbor::decode::{self, Decoder};
use minicbor::{Decode, Encode};
use std::fmt;
use std::str::FromStr;
use thiserror::Error;

/// The longest an address or a token id may be, in bytes.
pub const MAX_TEXT_BYTES: usize = 256;

/// Text above [`MAX_TEXT_BYTES`].
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("{len} bytes, above the cap of {MAX_TEXT_BYTES}")]
pub struct TextTooLong {
    pub len: usize,
}

macro_rules! text_type {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Encode)]
        #[cbor(transparent)]
        pub struct $name(#[n(0)] String);

        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        /// Reads the text and holds it to the bound `FromStr` holds it to, so nothing
        /// decoded from storage or a fixture can exceed it either.
        impl<'b, C> Decode<'b, C> for $name {
            fn decode(d: &mut Decoder<'b>, _ctx: &mut C) -> Result<Self, decode::Error> {
                d.str()?.parse().map_err(|error: TextTooLong| {
                    decode::Error::message(format!("{}: {error}", stringify!($name)))
                })
            }
        }

        impl FromStr for $name {
            type Err = TextTooLong;

            fn from_str(text: &str) -> Result<Self, Self::Err> {
                if text.len() > MAX_TEXT_BYTES {
                    return Err(TextTooLong { len: text.len() });
                }
                Ok(Self(text.to_string()))
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{:?}", self.0)
            }
        }
    };
}

text_type! {
    /// An account address on some chain, kept as the exact text it arrived as: parsing
    /// never changes case or checksum. At most [`MAX_TEXT_BYTES`].
    Address
}

text_type! {
    /// A token on some chain, kept as the exact text it arrived as. At most
    /// [`MAX_TEXT_BYTES`].
    TokenId
}

/// What a redacted secret prints as.
pub const REDACTED: &str = "***";

/// The redaction placeholder offered as a real rpc url.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("the redacted placeholder {REDACTED:?} is not an rpc url")]
pub struct RedactedRpcUrl;

/// An rpc provider url. The url carries the provider's api key, so `Debug` and `Display`
/// print [`REDACTED`]; [`RpcUrl::expose`] is the one way to the text.
#[derive(Clone, PartialEq, Eq, Encode, Decode)]
#[cbor(transparent)]
pub struct RpcUrl(#[n(0)] String);

impl RpcUrl {
    /// The secret url itself.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl FromStr for RpcUrl {
    type Err = RedactedRpcUrl;

    fn from_str(url: &str) -> Result<Self, Self::Err> {
        if url == REDACTED {
            return Err(RedactedRpcUrl);
        }
        Ok(Self(url.to_string()))
    }
}

impl fmt::Display for RpcUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

impl fmt::Debug for RpcUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}
