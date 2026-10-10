use crate::storage::events;
use ic_cdk::query;

#[query]
pub fn verify_replay() -> bool {
    events::verify_replay()
}
