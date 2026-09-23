//! The attestation inbox: what Circle attested for each swap's burn, handed in by the
//! watcher and taken out by the engine once the mint it feeds has delivered. The door
//! binds a pushed message to the swap before it lands here, and the rail binds it again
//! before it is minted, because a valid message of another burn would mint under this
//! swap's name: `receiveMessage` verifies the signatures on the chain, not whose burn they
//! are. What binds a message to its swap is the hook data the swap's own burn wrote into
//! it, the swap's quote hash, beside every other field the burn determined; two burns of
//! ours with the same parameters still emit two messages that name two swaps (see
//! `rails::cctp`). Not a fold of the event log, so it has a map of its own and the replay
//! audit does not compare it. Stable, so a pushed attestation survives an upgrade (rule
//! A9).
//!
//! The Eco intent inbox is this module's twin: both are swap-keyed stable maps outside the
//! fold, and they are kept apart rather than made one generic map because what a push
//! replaces differs (see each `put`), and because a map's memory id reads better beside
//! the type it holds than behind a macro.

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
