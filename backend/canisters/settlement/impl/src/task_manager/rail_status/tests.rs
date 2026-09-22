use super::*;

/// The engine's timer has a slot of its own, empty until `start_timers` wires it inside a
/// canister, so a restart replaces the timer rather than doubling the engine's rate.
#[test]
fn the_engine_has_its_own_timer_slot_and_it_starts_empty() {
    assert!(!is_wired());
}
