//! The finalized-tag chains' finalized blocks, as the outbox last read them.

use crate::storage::timer_state;
use ic_cdk::query;
pub use settlement_api::types::chain_data::FinalizedSeen;
use types::timer_state::TimerState;
use types::Timestamp;

#[cfg(test)]
mod tests;

/// Each chain that waits for its finalized block: the last finalized block a read gave, when
/// it last moved (IC time in nanoseconds, or when its clock last started), and whether it has
/// stood still long enough, while something waits, to be a stall. Nothing on a stalled chain
/// closes until it moves.
#[query]
pub fn finalized_seen() -> Vec<(u64, FinalizedSeen)> {
    seen_at(
        &timer_state::get(),
        Timestamp::from_nanos(ic_cdk::api::time()),
    )
}

/// What `finalized_seen` answers of `state` at `now`: a chain with nothing open (out of the
/// outbox's receipt schedule) is idle, and an idle chain is never stalled.
fn seen_at(state: &TimerState, now: Timestamp) -> Vec<(u64, FinalizedSeen)> {
    state
        .finalized_seen
        .iter()
        .map(|(chain, seen)| {
            (
                chain.get(),
                FinalizedSeen {
                    number: seen.number.map(|number| number.get()),
                    moved_at_ns: seen.moved_at.as_nanos(),
                    stalled: state.receipts_due.contains_key(chain) && seen.is_stalled(now),
                },
            )
        })
        .collect()
}
