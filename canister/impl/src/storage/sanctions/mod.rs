//! The sanctions set: addresses no swap may be claimed for. The watcher (or a controller)
//! pushes it; `claim_swap` reads it before it spends an outcall on a deposit, so a
//! sanctioned destination or refund address is refused with nothing read and nothing
//! written.
//!
//! Not a fold of the event log: it is compliance data pushed from outside, the same before
//! and after every event, so it has a map of its own and the replay audit does not compare
//! it. Bounded, because a service and not a controller writes it.

use crate::storage::memory::{sanctions_memory, Memory};
use ic_stable_structures::StableBTreeMap;
use std::cell::RefCell;
use thiserror::Error;
use types::{Address, EvmAddress};

/// The most addresses the set holds. Far above any sanctions list there is, and a backstop
/// against a compromised or looping watcher filling stable memory, not a business limit.
pub const MAX_SANCTIONED: u64 = 100_000;

/// Why a change to the set was refused. Nothing was written.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SanctionsError {
    #[error("the sanctions set is full at {capacity} addresses")]
    SetFull { capacity: u64 },
}

thread_local! {
    static SANCTIONED: RefCell<StableBTreeMap<Address, (), Memory>> =
        RefCell::new(StableBTreeMap::init(sanctions_memory()));
}

/// Writes the map's header in an update context, so no query is ever the first to grow its
/// memory.
pub fn init() {
    SANCTIONED.with(|_| ());
}

/// The one spelling the set holds an address under. An EVM address is twenty bytes and its
/// text is a spelling of them, so it is held in lower case and a checksummed or upper-case
/// spelling of the same bytes finds it; text that is not an EVM address is held as it came,
/// because a base58 address is another address in another case.
fn key(address: &Address) -> Address {
    match address.as_str().parse::<EvmAddress>() {
        Ok(_) => address
            .as_str()
            .to_ascii_lowercase()
            .parse()
            .expect("BUG: lower-casing ascii keeps the byte length"),
        Err(_) => address.clone(),
    }
}

pub fn is_sanctioned(address: &Address) -> bool {
    SANCTIONED.with(|set| set.borrow().contains_key(&key(address)))
}

/// How many addresses the set holds. `apply` answers the same number, so this is what the
/// tests read and nothing else.
#[cfg(test)]
pub(crate) fn len() -> u64 {
    SANCTIONED.with(|set| set.borrow().len())
}

/// Adds `add`, then removes `remove`, and answers how many addresses the set holds after.
/// The whole call is refused, with nothing written, when what it adds would take the set
/// past [`MAX_SANCTIONED`]: what it removes and what it re-adds is not growth, so a call
/// that swaps one address for another still goes through at the cap.
pub fn apply(add: &[Address], remove: &[Address]) -> Result<u64, SanctionsError> {
    SANCTIONED.with(|set| {
        let mut set = set.borrow_mut();
        let removed: std::collections::BTreeSet<Address> = remove.iter().map(key).collect();
        let new: std::collections::BTreeSet<Address> = add
            .iter()
            .map(key)
            .filter(|address| !set.contains_key(address))
            .collect();
        // what leaves is what the set holds now, is asked to go, and is not added back in
        // the same call: `new` holds only addresses the set does not have yet, so an
        // address that is in both lists and already held is counted here and not there
        let leaving = removed
            .iter()
            .filter(|address| set.contains_key(address) && !new.contains(*address))
            .count() as u64;
        let growth = new
            .iter()
            .filter(|address| !removed.contains(*address))
            .count() as u64;
        if set.len() - leaving + growth > MAX_SANCTIONED {
            return Err(SanctionsError::SetFull {
                capacity: MAX_SANCTIONED,
            });
        }
        for address in new {
            set.insert(address, ());
        }
        for address in removed {
            set.remove(&address);
        }
        Ok(set.len())
    })
}

#[cfg(test)]
mod tests;
