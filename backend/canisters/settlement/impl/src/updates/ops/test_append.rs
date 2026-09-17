use crate::guards::require_controller;
use crate::storage::events;
pub use settlement_api::types::errors::TestAppendError;
pub use settlement_api::types::events::EventType;

/// Test-only, controller-only door onto the log, through `append_event` like everything
/// else.
#[ic_cdk::update]
pub fn test_append(payload: EventType) -> Result<u64, TestAppendError> {
    require_controller().map_err(TestAppendError::Guard)?;
    let payload =
        types::EventType::try_from(payload).map_err(|e| TestAppendError::InvalidEvent(e.into()))?;
    events::append_event(payload)
        .map(|index| index.get())
        .map_err(|e| TestAppendError::Append(e.into()))
}
