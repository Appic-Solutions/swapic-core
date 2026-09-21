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
