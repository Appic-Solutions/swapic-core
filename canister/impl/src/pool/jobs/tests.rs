use super::*;

/// At most `AT_ONCE` tasks are out: the next asks for room and finds none, which is noted for
/// the next task to end, once; a task that ends gives its room back, a trap's drop included.
#[test]
fn at_most_at_once_tasks_are_out_and_a_full_drive_is_noted_once() {
    assert!(!take_skipped());
    let slots: Vec<Slot> = (0..AT_ONCE).map(|_| Slot::take().expect("room")).collect();
    assert_eq!(Slot::out(), AT_ONCE);
    assert!(Slot::take().is_none(), "no room past AT_ONCE");
    assert!(take_skipped(), "the drive that found no room is noted");
    assert!(!take_skipped(), "once");
    drop(slots);
    assert_eq!(Slot::out(), 0);
    let one = Slot::take().expect("room again");
    assert_eq!(Slot::out(), 1);
    drop(one);
    assert_eq!(Slot::out(), 0);
}
