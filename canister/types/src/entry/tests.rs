use super::*;
use ic_stable_structures::Storable;

/// A marker says what is in flight for the quote and since when, and knows when it has
/// outlived anything that could still be in flight: an outcall is capped at a minute and a
/// signature at a few seconds, so a marker older than the bound was left by a message that
/// never came back, and the next caller may take its place.
#[test]
fn a_marker_is_stale_once_it_has_outlived_what_it_marks() {
    let marker = InFlight {
        kind: InFlightKind::Claim,
        since: Timestamp::from_nanos(1_000_000_000),
    };
    let bound = Duration::from_secs(300);
    assert!(!marker.is_stale(Timestamp::from_nanos(300_999_999_999), bound));
    assert!(
        marker.is_stale(Timestamp::from_nanos(301_000_000_000), bound),
        "the whole bound has passed, and it is inclusive"
    );
    assert!(
        !marker.is_stale(Timestamp::from_nanos(0), bound),
        "a clock behind the stamp has waited no time at all"
    );
    assert_eq!(InFlight::from_bytes(marker.to_bytes()), marker);
    let pull = InFlight {
        kind: InFlightKind::Pull,
        since: Timestamp::from_nanos(7),
    };
    assert_eq!(InFlight::from_bytes(pull.to_bytes()), pull);
}

/// An attestation is what the watcher fetched from Circle for a burn: the message and the
/// signatures over it, each held to a bound, because the inbox is stable memory and a
/// service writes it.
#[test]
fn an_attestation_is_held_to_its_bounds_and_round_trips() {
    let at = Timestamp::from_nanos(1_700_000_000_000_000_000);
    let attestation = Attestation::new(vec![0xaa; 376], vec![0xbb; 130], at)
        .expect("a burn message and two signatures are inside the bounds");
    assert_eq!(attestation.message(), vec![0xaa; 376]);
    assert_eq!(attestation.attestation(), vec![0xbb; 130]);
    assert_eq!(attestation.received_at(), at);
    assert_eq!(Attestation::from_bytes(attestation.to_bytes()), attestation);

    assert_eq!(
        Attestation::new(vec![0; MAX_MESSAGE_BYTES + 1], vec![], at),
        Err(AttestationError::MessageTooLong {
            len: MAX_MESSAGE_BYTES + 1,
            cap: MAX_MESSAGE_BYTES
        })
    );
    assert_eq!(
        Attestation::new(vec![], vec![0; MAX_ATTESTATION_BYTES + 1], at),
        Err(AttestationError::AttestationTooLong {
            len: MAX_ATTESTATION_BYTES + 1,
            cap: MAX_ATTESTATION_BYTES
        })
    );
    assert!(Attestation::new(
        vec![0; MAX_MESSAGE_BYTES],
        vec![0; MAX_ATTESTATION_BYTES],
        at
    )
    .is_ok());
}
