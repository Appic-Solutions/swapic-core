use crate::task_manager;
use ic_cdk::query;

/// Test-only: whether a pass of the outbox is on its way. What lets the suite see that a
/// halted canister rests instead of re-arming every window, and that lifting the halt
/// wakes the pass.
#[query]
pub fn test_outbox_armed() -> bool {
    task_manager::outbox::is_armed()
}
