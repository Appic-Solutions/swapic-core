use super::*;
use types::swapic::EcoArrival;

fn address(byte: u8) -> EvmAddress {
    EvmAddress::new([byte; 20])
}

const NOW: u64 = 1_800_000_000;

fn intent() -> EcoIntent {
    EcoIntent {
        destination: 42_161,
        creator: address(1),
        token: address(2),
        reward_token: address(3),
        arrival: EcoArrival::Deliver {
            recipient: address(4),
        },
        prover: address(5),
        portal: address(6),
        route_deadline: NOW + 300,
        reward_deadline: NOW + 7 * 24 * 3_600,
    }
}

fn checked(intent: &EcoIntent) -> Result<(), String> {
    check_intent(intent, Some(address(6)), Some(address(5)), NOW + 120, NOW)
}

#[test]
fn an_intent_of_ecos_prover_and_portal_with_short_deadlines_passes() {
    assert_eq!(checked(&intent()), Ok(()));
}

#[test]
fn another_prover_portal_or_a_long_deadline_is_refused() {
    let refused = |intent: EcoIntent, words: &str| {
        let why = checked(&intent).unwrap_err();
        assert!(why.contains(words), "{why}");
    };
    refused(
        EcoIntent {
            prover: address(9),
            ..intent()
        },
        "another prover",
    );
    refused(
        EcoIntent {
            portal: address(9),
            ..intent()
        },
        "another Portal",
    );
    // a route filled before the order runs, or hours later
    refused(
        EcoIntent {
            route_deadline: NOW + 60,
            ..intent()
        },
        "route deadline",
    );
    refused(
        EcoIntent {
            route_deadline: NOW + 3_601,
            ..intent()
        },
        "route deadline",
    );
    refused(
        EcoIntent {
            reward_deadline: NOW + 9 * 24 * 3_600,
            ..intent()
        },
        "refund comes too late",
    );
    assert_eq!(
        check_intent(&intent(), Some(address(6)), None, NOW + 120, NOW),
        Err("no Eco prover is configured".to_string())
    );
}

/// L-03: an intent whose reward comes back less than an hour after its route can last be
/// filled is refused, so a refund never meets a fill in the seconds two chains' clocks
/// disagree; an hour on the dot passes.
#[test]
fn a_reward_back_within_the_hour_of_the_route_is_refused() {
    let route_deadline = intent().route_deadline;
    let why = checked(&EcoIntent {
        reward_deadline: route_deadline + MIN_REFUND_GAP_S - 1,
        ..intent()
    })
    .unwrap_err();
    assert!(why.contains("refund comes"), "{why}");
    assert_eq!(
        checked(&EcoIntent {
            reward_deadline: route_deadline + MIN_REFUND_GAP_S,
            ..intent()
        }),
        Ok(())
    );
}

/// The last review (item 4): one rule for when a pinned order's Eco intent can last be
/// filled, shared by the payouts' queue and a move's refund: its route's deadline, or, when
/// the route does not parse, its order's deadline and the longest route the canister signs.
#[test]
fn a_pinned_orders_fill_is_read_past_its_route_or_its_longest_route() {
    let pinned = |order: Vec<u8>| types::pool::PinnedOrder {
        order_id: [1; 32],
        order,
        gas_limit: 300_000,
        deadline_s: 900,
    };
    let route = crate::storage::pool::tests::eco_order(1_800);
    assert_eq!(fill_read_after(&pinned(route)), 1_800);
    assert_eq!(fill_read_after(&pinned(vec![7; 8])), 900 + MAX_ROUTE_LIFE_S);
}
