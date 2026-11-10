use super::*;

/// At most `AT_ONCE` tasks are out in a lane: the next asks for room and finds none, which is
/// noted for the next task to end, once; the other lane's room is its own; a task that ends
/// gives its room back, a trap's drop included.
#[test]
fn at_most_at_once_tasks_are_out_in_each_lane_and_a_full_drive_is_noted_once() {
    assert!(!take_skipped());
    let slots: Vec<Slot> = (0..AT_ONCE)
        .map(|_| Slot::take(Lane::Base).expect("room"))
        .collect();
    assert_eq!(Slot::out(Lane::Base), AT_ONCE);
    assert!(Slot::take(Lane::Base).is_none(), "no room past AT_ONCE");
    assert!(take_skipped(), "the drive that found no room is noted");
    assert!(!take_skipped(), "once");
    let icp = Slot::take(Lane::Icp).expect("the ICP lane has its own room");
    assert_eq!(Slot::out(Lane::Icp), 1);
    drop(icp);
    drop(slots);
    assert_eq!(Slot::out(Lane::Base), 0);
    assert_eq!(Slot::out(Lane::Icp), 0);
    assert!(start(Lane::Base, async {}));
    assert_eq!(Slot::out(Lane::Base), 0, "off the canister nothing runs, the room comes back");
}
