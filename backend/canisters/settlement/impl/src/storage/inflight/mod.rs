//! Rule A8: the markers the entry doors hold while a quote's claim or pull is out. A door
//! commits one before its first await, so a second call for the same quote is refused
//! instead of buying its outcalls a second time or signing a second pull, and removes it
//! when its message chain ends. Not a fold of the event log: a marker is scaffolding around one message chain and
//! describes nothing that happened to money.

use crate::deposits::MAX_READ_OUTCALLS;
use crate::storage::memory::{inflight_memory, Memory};
use ic_stable_structures::StableBTreeMap;
use std::cell::RefCell;
use std::time::Duration;
use types::{InFlight, InFlightKind, QuoteHash, Timestamp};

/// An HTTPS outcall's round trip on a loaded subnet: ten seconds, rounded up from the eight
/// seconds and more the review of fix wave 4 saw. The system lets one run to a minute, and
/// a bound sized at that would hold a quote for 49 minutes, past the shortest grace a claim
/// can be asked in; a marker taken over early costs duplicate outcalls and never money,
/// because the fold refuses a second `FundsReceived`.
pub const OUTCALL_ROUND_TRIP: Duration = Duration::from_secs(10);

/// How long a marker holds its quote before the next caller may take it over: the longest
/// a claim's reads can run, [`MAX_READ_OUTCALLS`] outcalls one after another (the head,
/// every batch of windows, every window again after a batch the provider refused, and the
/// deposit's block), at [`OUTCALL_ROUND_TRIP`] each: 490 seconds. A pull holds it for one
/// signing round trip of about ten seconds. A marker older than this was left behind by a
/// message that never came back, a trap after an await or an upgrade in the middle, and
/// holding the quote any longer would only keep its user waiting. It ends inside the
/// shortest grace a claim can be asked in (`MIN_CLAIM_GRACE`, ten minutes), so a marker a
/// trap left behind cannot outlast the claim's window.
pub const IN_FLIGHT_BOUND: Duration =
    Duration::from_secs(OUTCALL_ROUND_TRIP.as_secs() * MAX_READ_OUTCALLS);

/// Pinned so the unit test can say what the bound is sized from.
#[cfg(test)]
pub(crate) const READ_OUTCALLS: u64 = MAX_READ_OUTCALLS;

thread_local! {
    static IN_FLIGHT: RefCell<StableBTreeMap<QuoteHash, InFlight, Memory>> =
        RefCell::new(StableBTreeMap::init(inflight_memory()));
}

/// Writes the map's header in an update context, so no query is ever the first to grow its
/// memory.
pub fn init() {
    IN_FLIGHT.with(|_| ());
}

/// Takes the marker for `quote_hash` as `kind`, or answers the marker that already holds
/// it while that one is younger than [`IN_FLIGHT_BOUND`] at `now`. The caller reads the
/// clock, so a unit test runs without a canister.
pub fn take(quote_hash: QuoteHash, kind: InFlightKind, now: Timestamp) -> Result<(), InFlight> {
    IN_FLIGHT.with(|markers| {
        let mut markers = markers.borrow_mut();
        if let Some(held) = markers.get(&quote_hash) {
            if !held.is_stale(now, IN_FLIGHT_BOUND) {
                return Err(held);
            }
        }
        markers.insert(quote_hash, InFlight { kind, since: now });
        Ok(())
    })
}

/// Gives the quote back, whatever the message chain that held it ended in.
pub fn release(quote_hash: QuoteHash) {
    IN_FLIGHT.with(|markers| markers.borrow_mut().remove(&quote_hash));
}

/// Drops every marker. For `post_upgrade`: no message chain survives an upgrade, so
/// nothing the markers stood for is still out.
pub fn clear() {
    IN_FLIGHT.with(|markers| markers.borrow_mut().clear_new());
}

#[cfg(test)]
mod tests;
