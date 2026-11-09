//! The gas cache: one reading per chain, handed with the call that makes the canister sign
//! (`relay_gasless`, `finish_cctp`, `pay_payout`) or read by the canister itself, stamped
//! with the canister's clock and decided on only while it is younger than
//! [`READING_MAX_AGE`]. Nothing here is a fold of the event log, so it has a map of its own.

use crate::storage::memory::{chain_data_memory, Memory};
use ic_stable_structures::StableBTreeMap;
use std::cell::RefCell;
use std::time::Duration;
use types::chain_data::{ChainData, ChainReading};
use types::numeric::Timestamp;
use types::ChainId;

thread_local! {
    static CHAIN_DATA: RefCell<StableBTreeMap<ChainId, ChainData, Memory>> =
        RefCell::new(StableBTreeMap::init(chain_data_memory()));
}

/// Writes the map's header in an update context, so no query is ever the first to grow its
/// memory.
pub fn init() {
    CHAIN_DATA.with(|_| ());
}

/// Records what the watcher saw on `chain_id`, dated `at`: the caller hands over a reading
/// and the canister says when it arrived, because the age of a reading is what every
/// decision on it depends on. The caller reads the clock, so a unit test runs without a
/// canister.
pub fn put(chain_id: ChainId, reading: ChainReading, at: Timestamp) {
    // every reading, the canister's own or the watcher's, holds the chain's least tip
    let reading = reading.with_least_tip(chain_id);
    CHAIN_DATA.with(|cache| cache.borrow_mut().insert(chain_id, reading.pushed_at(at)));
}

/// The chain's entry, however old it is. For reading out, not for deciding on.
pub fn get(chain_id: ChainId) -> Option<ChainData> {
    CHAIN_DATA.with(|cache| cache.borrow().get(&chain_id))
}

/// The chain's entry while it is younger than [`READING_MAX_AGE`] at `now`, and nothing once it ages
/// out. The one door a money decision takes onto the cache, so a caller cannot decide on
/// stale gas prices by forgetting to check the stamp.
pub fn fresh(chain_id: ChainId, now: Timestamp) -> Option<ChainData> {
    get(chain_id).filter(|data| data.is_fresh(now, READING_MAX_AGE))
}

/// How long a reading prices a transaction after it arrived.
pub const READING_MAX_AGE: Duration = Duration::from_secs(10);

#[cfg(test)]
mod tests;
