use crate::storage::events;
use ic_cdk::query;
pub use settlement_api::types::events::Hash32;
pub use settlement_api::types::swap::SwapState;

/// The folded state of one swap. Public: every field of it is already in the
/// world-readable event log, `quote_bytes` included.
#[query]
pub fn get_swap(quote_hash: Hash32) -> Option<SwapState> {
    events::with_state(|s| s.swaps.get(&quote_hash).cloned())
}
