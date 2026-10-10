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

/// The marker holds a claim for as long as the longest read a claim can make takes: the
/// provider's head, every log read the read's budget allows, and the time of the
/// deposit's block, each at an outcall's round trip on a loaded subnet. And it gives the
/// quote back before the shortest grace a claim can be asked in has run, so a marker a
/// trap left behind never outlives the window in which the claim could still be asked.
///
/// Rewritten for fix wave 6 (H1): the log reads are a budget, 47, which holds both the
/// read whose every batch is refused (6 batches and 41 windows) and the read of a window
/// dust fills (every cap, then its halves down to the deposit); the count is unchanged.
#[test]
fn the_bound_fits_the_longest_claim_and_ends_inside_the_grace() {
    assert_eq!(READ_OUTCALLS, 49, "1 head, 47 log reads, 1 block");
    let longest = OUTCALL_ROUND_TRIP * u32::try_from(READ_OUTCALLS).unwrap();
    assert!(
        IN_FLIGHT_BOUND >= longest,
        "the bound {IN_FLIGHT_BOUND:?} holds the longest claim, {longest:?}"
    );
    assert!(
        IN_FLIGHT_BOUND < types::config::MIN_CLAIM_GRACE,
        "and ends inside the shortest grace"
    );
}
