use crate::storage::events;
use ic_cdk::query;
pub use settlement_api::types::events::Event;

/// `len` is capped server-side; ask for the count first and page through.
#[query]
pub fn events_page(start: u64, len: u64) -> Vec<Event> {
    events::events_page(start, len)
        .into_iter()
        .map(Event::from)
        .collect()
}
