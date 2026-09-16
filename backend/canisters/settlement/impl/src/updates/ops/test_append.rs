use crate::guards::require_controller;
use crate::storage::events;
pub use settlement_api::types::events::EventType;

/// Test-only door onto the log, behind a build feature and a controller check, and it
/// still goes through `append_event` like everything else.
#[ic_cdk::update]
pub fn test_append(payload: EventType) -> Result<u64, String> {
    require_controller()?;
    let payload = types::EventType::try_from(payload).map_err(|e| e.to_string())?;
    events::append_event(payload)
}
