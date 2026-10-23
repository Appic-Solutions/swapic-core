//! The per-caller limit's records ([`types::pool::callers`]): one per caller let through a
//! free door, in stable memory, so an upgrade resets no caller's count. A record is never
//! larger than the two log lines its caller's call wrote.

use crate::storage::memory::{pool_callers_memory, Memory};
use candid::Principal;
use ic_stable_structures::storable::Blob;
use ic_stable_structures::StableBTreeMap;
use std::cell::RefCell;
use types::pool::callers::CallerCalls;

/// A principal's bytes: at most 29.
type CallerKey = Blob<29>;

thread_local! {
    static CALLERS: RefCell<StableBTreeMap<CallerKey, CallerCalls, Memory>> =
        RefCell::new(StableBTreeMap::init(pool_callers_memory()));
}

/// Writes the map's header in an update context.
pub fn init() {
    CALLERS.with(|_| ());
}

fn key(caller: Principal) -> CallerKey {
    Blob::try_from(caller.as_slice()).expect("BUG: a principal is at most 29 bytes")
}

pub fn get(caller: Principal) -> Option<CallerCalls> {
    CALLERS.with(|callers| callers.borrow().get(&key(caller)))
}

pub fn set(caller: Principal, calls: CallerCalls) {
    CALLERS.with(|callers| callers.borrow_mut().insert(key(caller), calls));
}

#[cfg(test)]
pub(crate) fn len() -> u64 {
    CALLERS.with(|callers| callers.borrow().len())
}
