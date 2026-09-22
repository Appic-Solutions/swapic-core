use super::*;
use crate::storage::on_fresh_memory;
use types::Timestamp;

fn qh(byte: u8) -> QuoteHash {
    QuoteHash::new([byte; 32])
}

fn attestation(byte: u8) -> Attestation {
    Attestation::new(
        vec![byte; 376],
        vec![byte; 65],
        Timestamp::from_nanos(1_700_000_000_000_000_000),
    )
    .expect("inside the bounds")
}

/// One attestation per swap: a push lands, the same push again changes nothing, a
/// different one for the same swap replaces it (a corrected attestation must be usable),
/// and the engine takes it out when the mint is done.
#[test]
fn a_push_lands_replaces_and_is_taken_out() {
    on_fresh_memory(|| {
        init();
        assert_eq!(get(qh(1)), None);
        put(qh(1), attestation(1));
        assert_eq!(get(qh(1)), Some(attestation(1)));
        put(qh(1), attestation(1));
        assert_eq!(get(qh(1)), Some(attestation(1)), "idempotent");
        put(qh(1), attestation(2));
        assert_eq!(
            get(qh(1)),
            Some(attestation(2)),
            "a corrected push replaces"
        );
        assert_eq!(get(qh(2)), None, "another swap has none");
        remove(qh(1));
        assert_eq!(get(qh(1)), None);
    });
}
