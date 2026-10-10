//! The canister's own EVM address, derived once from its threshold key and kept.
//!
//! Not a fold of the event log: it is a fact about the key the canister holds, the same
//! before and after every event, so it has a cell of its own and the replay audit does not
//! compare it. Keys derive per canister id, so this value is fixed for the life of the
//! canister and the production canister id is fixed on day one.

use crate::storage::memory::{ecdsa_address_memory, Memory};
use ic_stable_structures::storable::{Bound, Storable};
use ic_stable_structures::StableCell;
use std::borrow::Cow;
use std::cell::RefCell;
use types::EvmAddress;

/// The address once it is known, and nothing before that. Stored as its twenty bytes, or
/// as no bytes at all, so the empty cell a fresh install writes reads as "not derived" and
/// the zero address stays a real address.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CachedAddress(pub Option<EvmAddress>);

impl Storable for CachedAddress {
    fn to_bytes(&self) -> Cow<'_, [u8]> {
        match self.0 {
            None => Cow::Borrowed(&[]),
            Some(address) => Cow::Owned(address.as_bytes().to_vec()),
        }
    }

    fn from_bytes(bytes: Cow<[u8]>) -> Self {
        match <[u8; 20]>::try_from(bytes.as_ref()) {
            Ok(address) => Self(Some(EvmAddress::new(address))),
            // the empty cell of a canister that has not derived yet; anything else is a
            // stored value this wasm does not read, and reading it as none would re-derive
            // the same address from the same key, so it is the safe reading either way
            Err(_) => Self(None),
        }
    }

    const BOUND: Bound = Bound::Bounded {
        max_size: 20,
        is_fixed_size: false,
    };
}

thread_local! {
    static ADDRESS: RefCell<StableCell<CachedAddress, Memory>> = RefCell::new(
        StableCell::init(ecdsa_address_memory(), CachedAddress::default())
            .expect("ecdsa address cell init"),
    );
}

/// Writes the empty cell in an update context, so no query is ever the first to grow its
/// memory.
pub fn init() {
    ADDRESS.with(|_| ());
}

pub fn get() -> Option<EvmAddress> {
    ADDRESS.with(|cell| cell.borrow().get().0)
}

/// Storage path; the caller decides what to derive. Out of stable memory is not a caller
/// error, so it traps rather than returning.
pub fn set(address: EvmAddress) {
    ADDRESS.with(|cell| {
        cell.borrow_mut()
            .set(CachedAddress(Some(address)))
            .expect("ecdsa address cell write")
    });
}

#[cfg(test)]
mod tests;
