use crate::storage::memory::{halt_memory, Memory};
use ic_stable_structures::StableCell;
use std::cell::RefCell;

thread_local! {
    // The halt flag: stable, because a halted canister must stay halted across the upgrade
    // that an operator reaches for first. There is no heap cache to drift from it.
    static HALTED: RefCell<StableCell<bool, Memory>> = RefCell::new(
        StableCell::init(halt_memory(), false).expect("halt cell init"),
    );
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
