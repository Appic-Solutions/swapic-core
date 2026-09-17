//! Stable storage encodings shared by the domain types.
//!
//! A stable map or log decodes a value only when it is read, so an upgrade that changes a
//! stored layout goes through and then traps on every read. `golden/storage_v1.txt` pins
//! the bytes of every stored type, and the test here decodes them with the current types.

#[cfg(test)]
mod tests;

/// Implements `Storable` for a minicbor type as its unbounded minicbor bytes. A stored
/// value that no longer decodes traps the read.
#[macro_export]
macro_rules! storable_as_cbor {
    ($t:ty) => {
        impl ::ic_stable_structures::Storable for $t {
            fn to_bytes(&self) -> ::std::borrow::Cow<'_, [u8]> {
                ::std::borrow::Cow::Owned(
                    ::minicbor::to_vec(self).expect("BUG: encoding into a Vec is infallible"),
                )
            }

            fn from_bytes(bytes: ::std::borrow::Cow<[u8]>) -> Self {
                ::minicbor::decode(&bytes)
                    .unwrap_or_else(|e| panic!("failed to decode a stored {}: {e}", stringify!($t)))
            }

            const BOUND: ::ic_stable_structures::storable::Bound =
                ::ic_stable_structures::storable::Bound::Unbounded;
        }
    };
}
