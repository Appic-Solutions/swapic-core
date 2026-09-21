//! The send queue: one entry per nonce that has been signed and has not landed.
//!
//! Not a fold of the event log, and deliberately so. The log says a transaction was
//! created, signed and replaced; how far its broadcast got is work in progress that no
//! event describes, so this map has a memory of its own and stays out of `State::matches`
//! and out of the replay audit's comparison. An entry leaves the moment its attempt closes
//! with `TxConfirmed` or `TxFailed`.

use crate::storage::memory::{outbox_memory, Memory};
use ic_stable_structures::StableBTreeMap;
use std::cell::RefCell;
use types::{ChainId, Nonce, OutboxEntry, OutboxKey, OutboxStatus};

thread_local! {
    static OUTBOX: RefCell<StableBTreeMap<OutboxKey, OutboxEntry, Memory>> =
        RefCell::new(StableBTreeMap::init(outbox_memory()));
}

/// Writes the map's header in an update context, so no query is ever the first to grow its
/// memory.
pub fn init() {
    OUTBOX.with(|_| ());
}

/// Writes `entry` at its own nonce, replacing whatever was there. A replacement at the
/// same nonce takes the same slot, so two transactions can never be in flight for one
/// nonce.
pub fn put(entry: OutboxEntry) {
    OUTBOX.with(|outbox| outbox.borrow_mut().insert(entry.key(), entry));
}

pub fn get(key: OutboxKey) -> Option<OutboxEntry> {
    OUTBOX.with(|outbox| outbox.borrow().get(&key))
}

/// Drops the entry, which is what closing an attempt does.
pub fn remove(key: OutboxKey) {
    OUTBOX.with(|outbox| outbox.borrow_mut().remove(&key));
}

pub fn is_empty() -> bool {
    OUTBOX.with(|outbox| outbox.borrow().is_empty())
}

/// Every chain that has an entry, in chain id order. The key orders by chain first, so
/// this is a walk of the map and never of the chains the config lists.
pub fn chains() -> Vec<ChainId> {
    let mut chains: Vec<ChainId> = Vec::new();
    OUTBOX.with(|outbox| {
        for (key, _) in outbox.borrow().iter() {
            if chains.last() != Some(&key.chain_id) {
                chains.push(key.chain_id);
            }
        }
    });
    chains
}

/// At most `limit` of the chain's entries in `status`, oldest nonce first, so a pass costs
/// the batch it works on and never the whole outbox.
pub fn by_status(chain_id: ChainId, status: OutboxStatus, limit: usize) -> Vec<OutboxEntry> {
    let range = OutboxKey {
        chain_id,
        nonce: Nonce::ZERO,
    }..=OutboxKey {
        chain_id,
        nonce: Nonce::new(u64::MAX),
    };
    OUTBOX.with(|outbox| {
        outbox
            .borrow()
            .range(range)
            .map(|(_, entry)| entry)
            .filter(|entry| entry.status == status)
            .take(limit)
            .collect()
    })
}

#[cfg(test)]
mod tests;
