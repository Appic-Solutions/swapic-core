use super::*;
use types::TokenAmount;

#[test]
fn the_books_are_read_after_a_movement_every_quarter_hour_and_while_a_payout_waits() {
    let at = Timestamp::from_secs(1_800_000_000).unwrap();
    let pockets = Pockets::default();
    assert!(due(None, at, pockets, false));
    let last = Some((at, pockets));
    let soon = at.checked_add(Duration::from_secs(60)).unwrap();
    assert!(!due(last, soon, pockets, false));
    assert!(due(last, soon, pockets, true));
    let mut moved = pockets;
    moved.base.free = TokenAmount::ONE;
    assert!(due(last, soon, moved, false));
    let later = at.checked_add(CHECK_EVERY).unwrap();
    assert!(due(last, later, pockets, false));
}
