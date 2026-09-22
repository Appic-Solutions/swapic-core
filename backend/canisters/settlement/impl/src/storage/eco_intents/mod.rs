//! The Eco intent inbox: what Eco's quote response gave each swap on the Eco rail, handed
//! in by the watcher and read by the rail for its publish and its reclaim. The vault locks
//! only the swap's own amount whatever the intent says, and a wrong route is an intent
//! nobody fills, which the deadline then refunds; what the intent decides beyond that (the
//! prover, the destination, the route itself) is why the rail stays off until its route is
//! designed, which `rails::eco` sets out. Not a fold of
//! the event log, so it has a map of its own and the replay audit does not compare it.
//! Stable, so a pushed intent survives an upgrade (rule A9). The attestation inbox is this
//! module's twin; the note there says why the two are kept apart.

use crate::storage::memory::{eco_intents_memory, Memory};
use ic_stable_structures::StableBTreeMap;
use std::cell::RefCell;
use types::{EcoIntent, QuoteHash};

thread_local! {
    static INTENTS: RefCell<StableBTreeMap<QuoteHash, EcoIntent, Memory>> =
        RefCell::new(StableBTreeMap::init(eco_intents_memory()));
}

/// Writes the map's header in an update context, so no query is ever the first to grow its
/// memory.
pub fn init() {
    INTENTS.with(|_| ());
}

/// Records `intent` for the swap, replacing whatever was there. The door refuses a push
/// once the swap has signed a leg, so what is replaced is only ever an intent no publish
/// carried: the reclaim then names the intent the Portal actually holds.
pub fn put(quote_hash: QuoteHash, intent: EcoIntent) {
    INTENTS.with(|inbox| inbox.borrow_mut().insert(quote_hash, intent));
}

pub fn get(quote_hash: QuoteHash) -> Option<EcoIntent> {
    INTENTS.with(|inbox| inbox.borrow().get(&quote_hash))
}

/// Drops the swap's intent, which is what the engine does once the swap has closed.
pub fn remove(quote_hash: QuoteHash) {
    INTENTS.with(|inbox| inbox.borrow_mut().remove(&quote_hash));
}

#[cfg(test)]
mod tests;
