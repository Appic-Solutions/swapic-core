use super::*;
use crate::chain::ChainId;
use crate::numeric::UnixSeconds;
use ic_stable_structures::Storable;

#[test]
fn every_rail_round_trips_through_its_id() {
    for (rail, id) in [
        (Rail::CctpV2Fast, "cctp_v2_fast"),
        (Rail::CctpV2Standard, "cctp_v2_standard"),
        (Rail::Eco, "eco"),
    ] {
        assert_eq!(rail.to_string(), id);
        assert_eq!(id.parse::<Rail>(), Ok(rail));
    }
}

#[test]
fn only_the_exact_ids_parse() {
    for id in ["", "CCTP_V2_FAST", "cctp_v2_fast ", "cctp", "rail \u{2603}"] {
        assert_eq!(id.parse::<Rail>(), Err(UnknownRail(id.to_string())));
    }
}

/// What the watcher hands in from Eco's quote response for a swap on the Eco rail: the
/// destination Eco named, the route, the reward's deadline and the prover, held to a
/// bound because the inbox is stable memory and a service writes it.
#[test]
fn an_eco_intent_is_held_to_its_bound_and_knows_its_deadline() {
    let intent = EcoIntent::new(
        ChainId::BASE,
        vec![0xde, 0xad],
        UnixSeconds::new(1_788_357_691),
        "0xeC00008537c1F26E739486BCFCC818d81234d5aD"
            .parse()
            .unwrap(),
    )
    .expect("a short route is inside the bound");
    assert_eq!(EcoIntent::from_bytes(intent.to_bytes()), intent);
    assert!(!intent.is_past_deadline(UnixSeconds::new(1_788_357_691)));
    assert!(
        intent.is_past_deadline(UnixSeconds::new(1_788_357_692)),
        "the deadline second is still the solver's"
    );
    assert_eq!(
        EcoIntent::new(
            ChainId::BASE,
            vec![0; MAX_ROUTE_BYTES + 1],
            UnixSeconds::new(1),
            "0xeC00008537c1F26E739486BCFCC818d81234d5aD"
                .parse()
                .unwrap(),
        ),
        Err(EcoIntentError::RouteTooLong {
            len: MAX_ROUTE_BYTES + 1,
            cap: MAX_ROUTE_BYTES
        })
    );
}
