use crate::storage::events;
use ic_cdk::query;
pub use settlement_api::types::events::Hash32;
pub use settlement_api::types::swap::Swap;
use types::QuoteHash;

/// The folded state of one swap. Public: every field of it is already in the
/// world-readable event log, `quote_bytes` included.
#[query]
pub fn get_swap(quote_hash: Hash32) -> Option<Swap> {
    let quote_hash = QuoteHash::new(quote_hash);
    events::with_state(|s| s.swaps.get(&quote_hash).cloned()).map(Swap::from)
}
