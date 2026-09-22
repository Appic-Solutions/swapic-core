#[cfg(test)]
mod tests;

use crate::evm::EvmAddress;
use ic_stable_structures::storable::{Bound, Storable};
use minicbor::decode::{self, Decoder};
use minicbor::{Decode, Encode};
use std::borrow::Cow;
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

/// Its utf8 bytes, so a stable map keyed by addresses orders them as text. Read back to the
/// bound parsing holds it to, which every stored value was held to when it was written.
impl Storable for Address {
    fn to_bytes(&self) -> Cow<'_, [u8]> {
        Cow::Borrowed(self.0.as_bytes())
    }

    fn from_bytes(bytes: Cow<[u8]>) -> Self {
        std::str::from_utf8(&bytes)
            .expect("BUG: a stored address is written as utf8")
            .parse()
            .expect("BUG: a stored address is written inside the bound")
    }

    const BOUND: Bound = Bound::Bounded {
        max_size: MAX_TEXT_BYTES as u32,
        is_fixed_size: false,
    };
}

text_type! {
    /// A token on some chain, kept as the exact text it arrived as. At most
    /// [`MAX_TEXT_BYTES`].
    TokenId
}

impl TokenId {
    /// Whether `other` names the token this one names. A token that is an EVM address is
    /// its twenty bytes, so two spellings of them (checksummed, lower case) are one token;
    /// text that is no address is held to its exact spelling, because a base58 address is
    /// another address in another case. What every comparison of a quote's token to the
    /// vault's, the rail's or a line's runs through, so no spelling can pass as another
    /// token or refuse the same one.
    pub fn names_the_same_token(&self, other: &TokenId) -> bool {
        match (
            self.as_str().parse::<EvmAddress>(),
            other.as_str().parse::<EvmAddress>(),
        ) {
            (Ok(mine), Ok(theirs)) => mine == theirs,
            _ => self == other,
        }
    }
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
