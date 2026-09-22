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

fn usdc_table() -> crate::config::ChainTable<EvmAddress> {
    crate::config::ChainTable(std::collections::BTreeMap::from([
        (
            ChainId::BASE,
            "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"
                .parse()
                .unwrap(),
        ),
        (
            ChainId::ARBITRUM,
            "0xaf88d065e77c8cC2239327C5EDb3A432268e5831"
                .parse()
                .unwrap(),
        ),
    ]))
}

/// Every rail carries the USDC the deploy configured and nothing else: a quote is on the
/// rail only when its source token is the source chain's USDC and its destination token the
/// destination chain's, compared as addresses and never as text. A quote naming any other
/// token, a token that is no address, or a chain the table does not name, is refused by
/// the field and the reason.
#[test]
fn the_rail_pins_both_tokens_to_the_configured_usdc() {
    use crate::quote::tests::fixed_quote;
    use crate::quote::{QuoteAddressError, QuoteAddressField};
    let table = usdc_table();
    let quote = fixed_quote();
    assert_eq!(ensure_rail_tokens(&table, &quote), Ok(()));
    let lower = crate::Quote {
        src_token: "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913"
            .parse()
            .unwrap(),
        ..fixed_quote()
    };
    assert_eq!(
        ensure_rail_tokens(&table, &lower),
        Ok(()),
        "a lower-case spelling is the same token"
    );

    let worthless: EvmAddress = "0x7551A66653f9a20979ed81835a0b7008EC83401b"
        .parse()
        .unwrap();
    let wrong_source = crate::Quote {
        src_token: worthless.to_string().parse().unwrap(),
        ..fixed_quote()
    };
    assert_eq!(
        ensure_rail_tokens(&table, &wrong_source),
        Err(RailTokenError::NotTheRailToken {
            field: QuoteAddressField::SrcToken,
            quoted: worthless,
            rail_token: table.get(ChainId::BASE).unwrap(),
            rail: Rail::CctpV2Fast,
        })
    );
    let wrong_destination = crate::Quote {
        dst_token: worthless.to_string().parse().unwrap(),
        rail: Rail::Eco,
        ..fixed_quote()
    };
    assert_eq!(
        ensure_rail_tokens(&table, &wrong_destination),
        Err(RailTokenError::NotTheRailToken {
            field: QuoteAddressField::DstToken,
            quoted: worthless,
            rail_token: table.get(ChainId::ARBITRUM).unwrap(),
            rail: Rail::Eco,
        }),
        "the Eco rail is pinned the same way"
    );
    let odd = crate::Quote {
        dst_token: "USDC".parse().unwrap(),
        ..fixed_quote()
    };
    assert_eq!(
        ensure_rail_tokens(&table, &odd),
        Err(RailTokenError::QuoteAddress(
            QuoteAddressError::NotAnAddress {
                field: QuoteAddressField::DstToken,
                reason: crate::evm::EvmAddressError::NoPrefix,
            }
        ))
    );
    let unnamed = crate::Quote {
        dst_chain: ChainId::POLYGON,
        ..fixed_quote()
    };
    assert_eq!(
        ensure_rail_tokens(&table, &unnamed),
        Err(RailTokenError::NoRailToken {
            chain_id: ChainId::POLYGON,
            rail: Rail::CctpV2Fast,
        })
    );
}
