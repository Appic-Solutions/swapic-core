use super::*;
use crate::storage::on_fresh_memory;
use types::InFlightKind;

fn at(secs: u64) -> Timestamp {
    Timestamp::from_secs(secs).expect("a test instant is inside the epoch")
}

fn qh(byte: u8) -> QuoteHash {
    QuoteHash::new([byte; 32])
}

/// Rule A8: the first caller takes the marker, the second is refused and told since when,
/// and once the first is done the quote is free again.
#[test]
fn a_marker_is_taken_once_and_given_back() {
    on_fresh_memory(|| {
        init();
        assert_eq!(take(qh(1), InFlightKind::Claim, at(100)), Ok(()));
        assert_eq!(
            take(qh(1), InFlightKind::Claim, at(101)),
            Err(InFlight {
                kind: InFlightKind::Claim,
                since: at(100)
            }),
            "the quote is in flight, and the refusal says since when"
        );
        assert_eq!(
            take(qh(1), InFlightKind::Pull, at(101)),
            Err(InFlight {
                kind: InFlightKind::Claim,
                since: at(100)
            }),
            "a pull is refused by a claim in flight: one thing at a time per quote"
        );
        assert_eq!(
            take(qh(2), InFlightKind::Pull, at(101)),
            Ok(()),
            "another quote is free"
        );
        release(qh(1));
        assert_eq!(take(qh(1), InFlightKind::Claim, at(102)), Ok(()));
    });
}

/// A marker left behind by a message that never came back (a trap after the outcall, an
/// upgrade in the middle) must not hold the quote forever: past the bound the next caller
/// takes its place, and inside it the marker still holds.
#[test]
fn a_stale_marker_is_taken_over_and_a_live_one_is_not() {
    on_fresh_memory(|| {
        init();
        assert_eq!(take(qh(1), InFlightKind::Claim, at(100)), Ok(()));
        let inside = at(100 + IN_FLIGHT_BOUND.as_secs() - 1);
        assert!(take(qh(1), InFlightKind::Claim, inside).is_err());
        let past = at(100 + IN_FLIGHT_BOUND.as_secs());
        assert_eq!(
            take(qh(1), InFlightKind::Pull, past),
            Ok(()),
            "the bound is inclusive, and the taker's own marker replaces the stale one"
        );
        assert_eq!(
            take(qh(1), InFlightKind::Claim, past),
            Err(InFlight {
                kind: InFlightKind::Pull,
                since: past
            })
        );
    });
}

/// Nothing is in flight after an upgrade: the message chains the markers stood for went
/// with the old heap, so the markers go too, and a retry is not made to wait out the bound.
#[test]
fn clearing_frees_every_quote() {
    on_fresh_memory(|| {
        init();
        take(qh(1), InFlightKind::Claim, at(100)).unwrap();
        take(qh(2), InFlightKind::Pull, at(100)).unwrap();
        clear();
        assert_eq!(take(qh(1), InFlightKind::Claim, at(100)), Ok(()));
        assert_eq!(take(qh(2), InFlightKind::Claim, at(100)), Ok(()));
    });
}
