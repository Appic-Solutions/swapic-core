//! The attestation inbox: what Circle attested for each swap's burn, handed in by the
//! watcher and taken out by the engine once the mint it feeds has landed. Rail data, not
//! money truth: `receiveMessage` verifies the pair on the chain, and a wrong one makes the
//! mint revert, which the receipt then says. Not a fold of the event log, so it has a map
//! of its own and the replay audit does not compare it. Stable, so a pushed attestation
//! survives an upgrade (rule A9).

use crate::storage::memory::{attestations_memory, Memory};
use ic_stable_structures::StableBTreeMap;
use std::cell::RefCell;
use types::{Attestation, QuoteHash};

thread_local! {
    static ATTESTATIONS: RefCell<StableBTreeMap<QuoteHash, Attestation, Memory>> =
        RefCell::new(StableBTreeMap::init(attestations_memory()));
}

/// Writes the map's header in an update context, so no query is ever the first to grow its
/// memory.
pub fn init() {
    ATTESTATIONS.with(|_| ());
}

/// Records `attestation` for the swap, replacing whatever was there: a corrected push must
/// be the one the mint carries.
pub fn put(quote_hash: QuoteHash, attestation: Attestation) {
    ATTESTATIONS.with(|inbox| inbox.borrow_mut().insert(quote_hash, attestation));
}

pub fn get(quote_hash: QuoteHash) -> Option<Attestation> {
    ATTESTATIONS.with(|inbox| inbox.borrow().get(&quote_hash))
}

/// Drops the swap's attestation, which is what the engine does once the mint has landed.
pub fn remove(quote_hash: QuoteHash) {
    ATTESTATIONS.with(|inbox| inbox.borrow_mut().remove(&quote_hash));
}

#[cfg(test)]
mod tests;
