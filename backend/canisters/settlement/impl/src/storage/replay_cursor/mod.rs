//! Where the deep audit resumes. The deep check folds the whole log onto the heap and
//! compares the result with the stable fold, and a log too long to fold in one message is
//! folded across many: between steps the fold so far lives here, in stable memory, so an
//! audit survives an upgrade in the middle. Canister plumbing rather than a fold of the
//! log, so it has a cell of its own and is not part of what the audit compares.

use crate::state::MemoryStore;
use crate::storage::memory::{replay_cursor_memory, Memory};
use ic_stable_structures::StableCell;
use minicbor::{Decode, Encode};
use std::cell::RefCell;

/// The fold of the log as far as the deep audit got. The index it resumes at and the hash
/// it links to are the fold's own ledger meta, so nothing here can disagree with it.
///
/// Stored as minicbor: `#[n]` indices are append-only, never renumbered or reused, and a
/// new field is optional. A saved fold that no longer decodes traps `storage::init`, which
/// refuses the upgrade and leaves the wasm that wrote it running.
#[derive(Clone, Debug, Default, PartialEq, Eq, Encode, Decode)]
pub struct ReplayCursor {
    #[n(0)]
    pub fold: MemoryStore,
}

impl ReplayCursor {
    /// The start of the log: the empty fold, which seals at index zero on the zero hash.
    pub fn genesis() -> Self {
        Self::default()
    }
}

types::storable_as_cbor!(ReplayCursor);

thread_local! {
    static CURSOR: RefCell<StableCell<ReplayCursor, Memory>> = RefCell::new(
        StableCell::init(replay_cursor_memory(), ReplayCursor::genesis())
            .expect("replay cursor init"),
    );
}

/// On a fresh install writes the genesis cursor to the cell, in an update context, so no
/// query is ever the first to grow its memory.
pub fn init() {
    CURSOR.with(|_| ());
}

pub fn get() -> ReplayCursor {
    CURSOR.with(|c| c.borrow().get().clone())
}

/// Storage path; the audit decides what to write. Out of stable memory is not a caller
/// error, so it traps rather than returning.
pub fn set(cursor: ReplayCursor) {
    CURSOR.with(|c| c.borrow_mut().set(cursor).expect("replay cursor write"));
}

#[cfg(test)]
mod tests;
