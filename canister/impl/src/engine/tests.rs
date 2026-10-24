use super::*;

#[test]
fn an_arming_in_the_same_round_coalesces_and_a_later_one_does_not() {
    let at = Timestamp::from_nanos(1_000);
    assert!(still_pending(Some(at), at));
    assert!(!still_pending(Some(at), Timestamp::from_nanos(1_001)));
    assert!(!still_pending(None, at));
}

/// A refused taker of a flow's guard built one and dropped it, which released the
/// holder's flow: a third taker then ran beside the first. It holds now.
#[test]
fn a_refused_taker_never_releases_a_busy_flow() {
    let flow = types::QuoteHash::new([0x42; 32]);
    let held = flows::Busy::take(flow).unwrap();
    assert!(flows::Busy::take(flow).is_none());
    assert!(flows::Busy::take(flow).is_none(), "still held");
    drop(held);
    assert!(flows::Busy::take(flow).is_some());
}
