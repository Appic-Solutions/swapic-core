use crate::guards::require_controller;
use crate::storage::events;
use crate::task_manager;
pub use settlement_api::types::errors::TestAppendError;
pub use settlement_api::types::events::EventType;

/// Test-only, controller-only door onto the log, through `append_event` like everything
/// else.
///
/// An appended line arms the outbox pass, because the endpoints that write these lines for
/// real do: `create_and_send` arms as soon as it has allocated a nonce, so a line planted
/// here leaves the canister in the state the real path would leave it in and not one round
/// behind it.
#[ic_cdk::update]
pub fn test_append(payload: EventType) -> Result<u64, TestAppendError> {
    require_controller().map_err(TestAppendError::Guard)?;
    let payload =
        types::EventType::try_from(payload).map_err(|e| TestAppendError::InvalidEvent(e.into()))?;
    let index = events::append_event(payload)
        .map(|index| index.get())
        .map_err(|e| TestAppendError::Append(e.into()))?;
    task_manager::outbox::arm_if_work_is_pending();
    Ok(index)
}
