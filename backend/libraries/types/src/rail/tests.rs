use super::*;

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
