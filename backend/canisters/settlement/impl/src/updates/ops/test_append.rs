use crate::guards::require_controller;
use crate::storage::events;
pub use settlement_api::types::events::Event;

/// Test-only door onto the log, behind a build feature and a controller check, and it
/// still goes through `append_event` like everything else.
#[ic_cdk::update]
pub fn test_append(event: Event) -> Result<u64, String> {
    require_controller()?;
    events::append_event(event)
}
