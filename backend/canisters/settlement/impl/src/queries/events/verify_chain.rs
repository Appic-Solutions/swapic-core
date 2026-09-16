use crate::storage::events;
use ic_cdk::query;

#[query]
pub fn verify_chain() -> bool {
    events::verify_chain()
}
