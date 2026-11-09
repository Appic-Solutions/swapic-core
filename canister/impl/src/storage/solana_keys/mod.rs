//! The canister's two Solana addresses, derived once from its threshold Ed25519 key and kept, as
//! the EVM address is: a fact about the key, the same before and after every event.

use crate::storage::memory::{solana_keys_memory, Memory};
use ic_stable_structures::storable::{Bound, Storable};
use ic_stable_structures::StableCell;
use std::borrow::Cow;
use std::cell::RefCell;

/// The payer key and the admin key once derived. Stored as their 64 bytes, or as none at all, so the
/// empty cell of a fresh canister reads as "not derived".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CachedKeys(pub Option<([u8; 32], [u8; 32])>);

impl Storable for CachedKeys {
    fn to_bytes(&self) -> Cow<'_, [u8]> {
        match self.0 {
            None => Cow::Borrowed(&[]),
            Some((payer, admin)) => {
                let mut bytes = Vec::with_capacity(64);
                bytes.extend_from_slice(&payer);
                bytes.extend_from_slice(&admin);
                Cow::Owned(bytes)
            }
        }
    }

    fn from_bytes(bytes: Cow<[u8]>) -> Self {
        if bytes.len() != 64 {
            return Self(None);
        }
        let mut payer = [0u8; 32];
        let mut admin = [0u8; 32];
        payer.copy_from_slice(&bytes[..32]);
        admin.copy_from_slice(&bytes[32..]);
        Self(Some((payer, admin)))
    }

    const BOUND: Bound = Bound::Bounded {
        max_size: 64,
        is_fixed_size: false,
    };
}

thread_local! {
    static KEYS: RefCell<StableCell<CachedKeys, Memory>> = RefCell::new(
        StableCell::init(solana_keys_memory(), CachedKeys::default()).expect("solana keys cell init"),
    );
}

pub fn get() -> Option<([u8; 32], [u8; 32])> {
    KEYS.with(|cell| cell.borrow().get().0)
}

pub fn set(payer: [u8; 32], admin: [u8; 32]) {
    KEYS.with(|cell| {
        cell.borrow_mut()
            .set(CachedKeys(Some((payer, admin))))
            .expect("solana keys cell write");
    });
}
