use crate::storage::memory::{halt_memory, Memory};
use ic_stable_structures::StableCell;
use std::cell::RefCell;

thread_local! {
    // A halted canister must stay halted across the upgrade an operator reaches for first.
    static HALTED: RefCell<StableCell<bool, Memory>> = RefCell::new(
        StableCell::init(halt_memory(), false).expect("halt cell init"),
    );
}

/// On a fresh install writes the flag to the cell, in an update context, so no query is
/// ever the first to grow its memory.
pub fn init() {
    HALTED.with(|_| ());
}

pub fn is_halted() -> bool {
    HALTED.with(|h| *h.borrow().get())
}

/// Storage path; the caller does the authorization. True is an emergency stop, false is a
/// human saying the divergence the audit found has been explained.
pub fn set_halted(halted: bool) {
    // out of stable memory is not a caller error, so it traps rather than returning
    HALTED.with(|h| h.borrow_mut().set(halted).expect("halt cell write"));
}
