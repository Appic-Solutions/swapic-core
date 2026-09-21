//! Where the rolling chain audit resumes. The audit verifies a bounded chunk of the log per
//! tick, so it has to remember two things between ticks: the next index to verify, and the
//! hash the entry before it sealed. Canister plumbing rather than a fold of the log, so it
//! has a cell of its own and is not part of what the audit compares.

use crate::storage::memory::{audit_cursor_memory, Memory};
use ic_stable_structures::storable::{Bound, Storable};
use ic_stable_structures::StableCell;
use std::borrow::Cow;
use std::cell::RefCell;
use types::{EventHash, EventIndex};

/// The next link the audit checks, and the hash it checks that link's parent against.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AuditCursor {
    pub next_index: EventIndex,
    pub parent_hash: EventHash,
}

impl AuditCursor {
    /// The start of the chain: index zero, anchored on the zero hash, which is what
    /// index 0 links to.
    pub const GENESIS: Self = Self {
        next_index: EventIndex::ZERO,
        parent_hash: EventHash::ZERO,
    };
}

/// Forty bytes: the eight big-endian bytes of the index, then the hash. Fixed size, so the
/// cell never grows, and a cursor that no longer decodes traps the read rather than
/// silently restarting the audit somewhere else.
impl Storable for AuditCursor {
    fn to_bytes(&self) -> Cow<'_, [u8]> {
        let mut bytes = Vec::with_capacity(40);
        bytes.extend_from_slice(&self.next_index.get().to_be_bytes());
        bytes.extend_from_slice(self.parent_hash.as_ref());
        Cow::Owned(bytes)
    }

    fn from_bytes(bytes: Cow<[u8]>) -> Self {
        let (index, parent) = bytes
            .split_first_chunk::<8>()
            .expect("BUG: a stored audit cursor is written as exactly 40 bytes");
        Self {
            next_index: EventIndex::new(u64::from_be_bytes(*index)),
            parent_hash: EventHash::new(
                parent
                    .try_into()
                    .expect("BUG: a stored audit cursor is written as exactly 40 bytes"),
            ),
        }
    }

    const BOUND: Bound = Bound::Bounded {
        max_size: 40,
        is_fixed_size: true,
    };
}

thread_local! {
    static CURSOR: RefCell<StableCell<AuditCursor, Memory>> = RefCell::new(
        StableCell::init(audit_cursor_memory(), AuditCursor::GENESIS).expect("audit cursor init"),
    );
}

/// On a fresh install writes the genesis cursor to the cell, in an update context, so no
/// query is ever the first to grow its memory.
pub fn init() {
    CURSOR.with(|_| ());
}

pub fn get() -> AuditCursor {
    CURSOR.with(|c| *c.borrow().get())
}

/// Storage path; the audit decides what to write. Out of stable memory is not a caller
/// error, so it traps rather than returning.
pub fn set(cursor: AuditCursor) {
    CURSOR.with(|c| c.borrow_mut().set(cursor).expect("audit cursor write"));
}

#[cfg(test)]
mod tests;
