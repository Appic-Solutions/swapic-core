//! The finalized-tag chains' finalized blocks, as the outbox last read them.

use crate::storage::timer_state;
use ic_cdk::query;
pub use settlement_api::types::chain_data::FinalizedSeen;
use types::Timestamp;

/// Each chain that waits for its finalized block: the last finalized block a read gave, when
/// it last moved (IC time in nanoseconds, or when its clock last started), and whether it has
/// stood still long enough, while something waits, to be a stall. Nothing on a stalled chain
/// closes until it moves.
#[query]
pub fn finalized_seen() -> Vec<(u64, FinalizedSeen)> {
    let now = Timestamp::from_nanos(ic_cdk::api::time());
    timer_state::get()
        .finalized_seen
        .into_iter()
        .map(|(chain, seen)| {
            (
                chain.get(),
                FinalizedSeen {
                    number: seen.number.map(|number| number.get()),
                    moved_at_ns: seen.moved_at.as_nanos(),
                    stalled: seen.is_stalled(now),
                },
            )
        })
        .collect()
}
