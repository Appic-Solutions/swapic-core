use crate::guards::require_controller;
use crate::storage::events;

/// Test-only door onto the append chokepoint's head check: it flips one bit of the stable
/// fold's chain head and leaves the log untouched, which is the fold-versus-log divergence
/// the check and the replay audit exist for and which nothing else can produce. Behind the same
/// build feature and controller check as `test_append`, so neither reaches the production
/// interface. Calling it twice puts the head back.
#[ic_cdk::update]
pub fn test_skew_state() -> Result<(), String> {
    require_controller()?;
    events::test_skew_chain_head();
    Ok(())
}
