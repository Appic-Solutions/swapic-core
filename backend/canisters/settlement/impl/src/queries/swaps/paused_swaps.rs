use crate::engine::{self, MAX_PAUSED_PAGE};
use crate::storage::{config, events};
use ic_cdk::query;
use settlement_api::types::events::Hash32;
pub use settlement_api::types::swap::PausedSwapsPage;
use types::QuoteHash;

/// World-readable, and bounded. How many open swaps wait on a rail the deploy has turned
/// off: each is paused, not stopped, refused with a retryable `RailDisabled` on every
/// engine tick, and moves again once the rail is back on. Counted a page at a time: one
/// call reads at most 500 swaps, in swap id order, starting after `after` (or at the first
/// swap), so it answers however long the history grows. The whole count is the sum of
/// `paused` over the pages, each asked with the `next` the one before it answered, until a
/// page answers none. A count and nothing else, so it says nothing about any one swap that
/// the public event log does not already, and a call costs its page, which is why it
/// stays open to anyone.
#[query]
pub fn paused_swaps(after: Option<Hash32>) -> PausedSwapsPage {
    let config = config::get();
    let page = events::read_state(|state| {
        engine::paused_page(
            &config,
            state.store(),
            after.map(QuoteHash::new),
            MAX_PAUSED_PAGE,
        )
    });
    PausedSwapsPage {
        paused: page.paused,
        read: page.read,
        next: page.next.map(QuoteHash::into_bytes),
    }
}
