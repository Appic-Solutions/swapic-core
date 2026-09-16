#[cfg(test)]
mod tests;

use minicbor::{Decode, Encode};
use std::fmt;

macro_rules! hash_type {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Encode, Decode)]
        #[cbor(transparent)]
        pub struct $name(#[cbor(n(0), with = "minicbor::bytes")] [u8; 32]);

        impl $name {
            pub const fn new(bytes: [u8; 32]) -> Self {
                Self(bytes)
            }

            pub const fn into_bytes(self) -> [u8; 32] {
                self.0
            }
        }

        impl From<[u8; 32]> for $name {
            fn from(bytes: [u8; 32]) -> Self {
                Self(bytes)
            }
        }

        impl AsRef<[u8; 32]> for $name {
            fn as_ref(&self) -> &[u8; 32] {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", hex::encode(self.0))
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", hex::encode(self.0))
            }
        }
    };
}

hash_type! {
    /// The swap id: sha256 over a quote's canonical preimage.
    QuoteHash
}

hash_type! {
    /// The hash of an EVM transaction.
    TxHash
}

hash_type! {
    /// A link of the event log's hash chain.
    EventHash
}

impl EventHash {
    /// The parent of the first event.
    pub const ZERO: Self = Self([0; 32]);
}
