//! The flows' guard: a flow a door is acting on right now is busy, so no second door or
//! drive acts on it at the same time. The ABI v2 ICP flows' drivers it held are gone with
//! their doors; the pool replaces them, and the relay and the CCTP finish are moved
//! by the outbox.

use std::cell::RefCell;
use std::collections::BTreeSet;
use types::QuoteHash;

thread_local! {
    // Heap by necessity: the flows a door is in the middle of right now. An upgrade waits
    // for every outstanding call, so nothing in here survives one.
    static BUSY: RefCell<BTreeSet<QuoteHash>> = const { RefCell::new(BTreeSet::new()) };
}

/// Held while a door acts on one flow, released on drop, a trap included.
pub struct Busy(QuoteHash);

impl Busy {
    pub fn take(quote_hash: QuoteHash) -> Option<Self> {
        // lazily: a guard built for a refusal would release the holder's flow on its drop
        BUSY.with(|busy| busy.borrow_mut().insert(quote_hash))
            .then(|| Self(quote_hash))
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        BUSY.with(|busy| busy.borrow_mut().remove(&self.0));
    }
}
