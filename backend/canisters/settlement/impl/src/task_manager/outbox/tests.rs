use super::*;

/// Rule A7: a pass that is running holds the only guard there is, and the guard comes back
/// when it drops, including when the pass traps and unwinds through it.
#[test]
fn one_pass_runs_at_a_time_and_the_guard_comes_back_on_a_trap() {
    let first = PassGuard::take();
    assert!(first.is_some(), "the first pass takes the guard");
    assert!(
        PassGuard::take().is_none(),
        "a second pass finds the guard taken"
    );
    drop(first);
    let again = PassGuard::take();
    assert!(again.is_some(), "the guard comes back when a pass ends");
    drop(again);

    let unwound = std::panic::catch_unwind(|| {
        let _guard = PassGuard::take().expect("the guard is free");
        panic!("the pass traps");
    });
    assert!(unwound.is_err());
    assert!(
        PassGuard::take().is_some(),
        "a trap releases the guard on the way out"
    );
}

/// A timer whose window has passed has fired, whatever the slot still holds. The callback
/// is its own message, so a trap inside it before its first await rolls back everything it
/// did, including clearing the slot; a rule that read only "is there an id" would then see
/// a timer that had already fired and never arm another pass, and the outbox would stop
/// until the next upgrade.
#[test]
fn a_timer_whose_window_has_passed_no_longer_holds_the_slot() {
    let now = Timestamp::from_nanos(10_000_000_000);
    let earlier = Timestamp::from_nanos(9_000_000_000);
    let later = Timestamp::from_nanos(11_000_000_000);

    assert!(
        !still_waiting(None, now),
        "nothing armed is nothing waiting"
    );
    assert!(
        still_waiting(Some(later), now),
        "a timer due later is still on its way"
    );
    assert!(
        !still_waiting(Some(earlier), now),
        "a timer due earlier has fired, whatever the slot holds"
    );
    assert!(
        !still_waiting(Some(now), now),
        "and one due exactly now is firing, so the next pass is armed on its own window"
    );
}
