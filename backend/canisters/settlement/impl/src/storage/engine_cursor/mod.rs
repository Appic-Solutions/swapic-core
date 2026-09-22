//! Where the engine's last tick stopped: the swap id it ended on, so the next tick starts
//! after it and wraps.
//!
//! A tick acts on at most `MAX_SWAPS_PER_TICK` swaps, and the store walks them in swap id
//! order, so without a cursor a backlog of more than that would hand every tick the same
//! low ids and starve every swap behind them. The cursor is a place in that order and not
//! a swap: the swap it names may close, and the next tick then begins at the next id
//! after it.
//!
//! Not a fold of the event log: it says nothing about any swap, only where a pass got to,
//! so it has a cell of its own and the replay audit does not compare it. Stable, so a tick
//! interrupted by an upgrade does not send the next one back to the start.

use crate::storage::memory::{engine_cursor_memory, Memory};
use ic_stable_structures::storable::{Bound, Storable};
use ic_stable_structures::StableCell;
use std::borrow::Cow;
use std::cell::RefCell;
use types::QuoteHash;

/// The last swap a tick drove, and nothing before the first tick. Stored as the thirty-two
/// bytes of the id, or as no bytes at all, so the empty cell a fresh install writes reads
/// as "start at the beginning".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cursor(pub Option<QuoteHash>);

impl Storable for Cursor {
    fn to_bytes(&self) -> Cow<'_, [u8]> {
        match self.0 {
            None => Cow::Borrowed(&[]),
            Some(quote_hash) => Cow::Owned(quote_hash.as_ref().to_vec()),
        }
    }

    fn from_bytes(bytes: Cow<[u8]>) -> Self {
        match <[u8; 32]>::try_from(bytes.as_ref()) {
            Ok(quote_hash) => Self(Some(QuoteHash::new(quote_hash))),
            // the empty cell of a canister whose engine has not ticked yet; anything else
            // is a stored value this wasm does not read, and starting from the beginning
            // is the safe reading either way
            Err(_) => Self(None),
        }
    }

    const BOUND: Bound = Bound::Bounded {
        max_size: 32,
        is_fixed_size: false,
    };
}

thread_local! {
    static CURSOR: RefCell<StableCell<Cursor, Memory>> = RefCell::new(
        StableCell::init(engine_cursor_memory(), Cursor::default())
            .expect("engine cursor cell init"),
    );
}

/// Writes the empty cell in an update context, so no query is ever the first to grow its
/// memory.
pub fn init() {
    CURSOR.with(|_| ());
}

/// The swap the last tick ended on, if there was one.
pub fn get() -> Option<QuoteHash> {
    CURSOR.with(|cell| cell.borrow().get().0)
}

/// Records where this tick stopped. Out of stable memory is not a caller error, so it
/// traps rather than returning.
pub fn set(quote_hash: QuoteHash) {
    CURSOR.with(|cell| {
        cell.borrow_mut()
            .set(Cursor(Some(quote_hash)))
            .expect("engine cursor cell write")
    });
}

#[cfg(test)]
mod tests;
