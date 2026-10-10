use crate::storage::events;
use ic_cdk::query;

#[query]
pub fn event_count() -> u64 {
    events::event_count()
}
