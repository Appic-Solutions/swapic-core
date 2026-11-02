use super::*;

#[test]
fn a_tick_is_pending_only_while_its_instant_lies_ahead() {
    let at = Timestamp::from_nanos;
    assert!(!still_pending(None, at(5)));
    assert!(still_pending(Some(at(6)), at(5)));
    assert!(
        !still_pending(Some(at(5)), at(5)),
        "a tick firing now is not pending"
    );
    assert!(
        !still_pending(Some(at(4)), at(5)),
        "nor one a trap left behind"
    );
}
