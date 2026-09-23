use super::*;
use crate::rails::tests::{at, config, fixture_swap, quote, USDC_BASE, VAULT_BASE};
use crate::rails::{RailStep, ReclaimStep, WaitingFor};
use types::abi::{
    decode_eco_publish_and_fund, decode_eco_refund, decode_vault_execute, EcoPublish, EcoReclaim,
    VaultExecution,
};
use types::config::EcoEnabled;
use types::{ChainId, Config, EvmAddress, Outcome, Rail, TokenAmount, UnixSeconds};

const PROVER: &str = "0xeC00008537c1F26E739486BCFCC818d81234d5aD";

/// An intent as Eco quoted it for a Base to Arbitrum swap: the two-hop trap, where Eco
/// fills on Base itself and the destination it names is Base.
fn intent(deadline: u64) -> EcoIntent {
    EcoIntent::new(
        ChainId::BASE,
        vec![0xde, 0xad, 0xbe, 0xef],
        UnixSeconds::new(deadline),
        PROVER.parse().unwrap(),
    )
    .unwrap()
}

/// The two-hop trap: the intent is published for the destination Eco's quote named, which
/// for a Base to Arbitrum swap is Base itself, and never for the chain the user is paid on.
/// The reward is the swap's whole amount of the source USDC, locked until the quoted
/// deadline, and its creator is the vault, whoever the watcher says.
#[test]
fn the_publish_names_ecos_destination_and_the_vault_as_the_rewards_creator() {
    let quote = quote();
    assert_eq!(
        quote.dst_chain,
        ChainId::ARBITRUM,
        "the user is paid on Arbitrum"
    );
    let swap = fixture_swap(None, None);
    let config = config();
    let intent = intent(1_788_357_691);
    let at = at(&quote, &swap, &config, None, Some(&intent));
    let publish = Eco.publish(&at, &intent).expect("everything is configured");
    assert_eq!(publish.purpose, TxPurpose::Burn(at.quote_hash));
    assert_eq!(publish.chain_id, ChainId::BASE);
    assert_eq!(publish.to, VAULT_BASE.parse::<EvmAddress>().unwrap());
    assert_eq!(publish.gas_limit, PUBLISH_GAS_LIMIT);
    let VaultExecution {
        swap_ref,
        calls,
        deltas,
    } = decode_vault_execute(&publish.data).expect("an execute");
    assert_eq!(swap_ref, at.quote_hash);
    let usdc_base: EvmAddress = USDC_BASE.parse().unwrap();
    assert_eq!(calls[0].target, config.eco_portal.unwrap());
    assert_eq!(calls[0].approve_token, usdc_base);
    assert_eq!(calls[0].approve_amount, swap.amount_in);
    let EcoPublish {
        destination,
        route,
        reward,
    } = decode_eco_publish_and_fund(&calls[0].data).expect("a publishAndFund");
    assert_eq!(
        destination,
        ChainId::BASE,
        "Eco's destination, not the swap's"
    );
    assert_eq!(route, vec![0xde, 0xad, 0xbe, 0xef]);
    assert_eq!(
        reward,
        EcoReward {
            deadline: UnixSeconds::new(1_788_357_691),
            creator: VAULT_BASE.parse().unwrap(),
            prover: PROVER.parse().unwrap(),
            native_amount: Wei::ZERO,
            tokens: vec![(usdc_base, TokenAmount::from(25_000_000_u32))],
        }
    );
    assert_eq!(deltas[0].min_change, -25_000_000);
}

/// The rail's table: nothing moves until the watcher hands in the intent; then the
/// publish; then, with the publish confirmed, the engine reads the destination vault for
/// the arrival, and once the deadline is past a read that finds nothing ends the swap.
/// Reclaiming waits for the deadline and is then the Portal's permissionless refund from
/// this canister's own address, naming the route by its hash.
#[test]
fn the_steps_run_intent_publish_arrival_and_reclaim_after_the_deadline() {
    let quote = quote();
    let config = config();
    let fresh = fixture_swap(None, None);
    assert_eq!(
        Eco.step(&at(&quote, &fresh, &config, None, None)),
        Ok(RailStep::Wait(WaitingFor::Intent))
    );
    let intent = intent(2_000);
    assert!(matches!(
        Eco.step(&at(&quote, &fresh, &config, None, Some(&intent))),
        Ok(RailStep::Send(RailTx {
            purpose: TxPurpose::Burn(_),
            ..
        }))
    ));

    let published = fixture_swap(Some(SwapLeg::Burn), Some(Outcome::Confirmed));
    let mut before = at(&quote, &published, &config, None, Some(&intent));
    before.now = UnixSeconds::new(2_000);
    assert_eq!(
        Eco.step(&before),
        Ok(RailStep::CheckArrival {
            chain_id: ChainId::ARBITRUM,
            expired: false
        })
    );
    let mut after = at(&quote, &published, &config, None, Some(&intent));
    after.now = UnixSeconds::new(2_001);
    assert_eq!(
        Eco.step(&after),
        Ok(RailStep::CheckArrival {
            chain_id: ChainId::ARBITRUM,
            expired: true
        })
    );

    assert_eq!(
        Eco.reclaim(&before),
        Ok(ReclaimStep::Wait(WaitingFor::Deadline)),
        "the reward is the filler's until the deadline"
    );
    let reclaim = match Eco.reclaim(&after) {
        Ok(ReclaimStep::Send(tx)) => tx,
        other => panic!("past the deadline the reward is reclaimed: {other:?}"),
    };
    assert_eq!(reclaim.purpose, TxPurpose::Reclaim(after.quote_hash));
    assert_eq!(reclaim.chain_id, ChainId::BASE);
    assert_eq!(reclaim.to, config.eco_portal.unwrap());
    assert_eq!(reclaim.gas_limit, RECLAIM_GAS_LIMIT);
    // the bytes are read back into the call they make, not matched by their prefix
    assert_eq!(
        decode_eco_refund(&reclaim.data),
        Some(EcoReclaim {
            destination: ChainId::BASE,
            route_hash: eco_route_hash(&[0xde, 0xad, 0xbe, 0xef]),
            reward: Eco.reward(&after, &intent).unwrap(),
        })
    );
    let without_intent = at(&quote, &published, &config, None, None);
    assert!(matches!(
        Eco.reclaim(&without_intent),
        Ok(ReclaimStep::Stuck(_))
    ));

    let no_portal = Config {
        eco_portal: None,
        ..config.clone()
    };
    assert_eq!(
        Eco.step(&at(&quote, &fresh, &no_portal, None, Some(&intent)))
            .map(drop),
        Err(RailError::NoEcoPortal)
    );
}

/// The rail is off until its route is designed (see the module doc for what must be true
/// first), so with the knob at its default it moves nothing at all: no publish, and no
/// reclaim of a swap that published while the knob was on. Both answer the same retryable
/// refusal, so turning the knob off pauses every swap on the rail and turning it back on
/// resumes them: a knob flip never freezes a swap for good, and the reward locked in the
/// Portal stays refundable. The paused swaps are counted by the `paused_swaps` query.
///
/// Rewritten for fix wave 4 (N5, finding 7): the reclaim answered `RailDisabled`, which the
/// engine retried each tick without ever stopping the swap.
///
/// Rewritten again for fix wave 5 (L3): the reclaim answered `Stuck`, which froze a
/// refunding swap for good on a minute's knob flip while an executing one only paused, so
/// it answers `RailDisabled` like the step again, and the pause is surfaced instead.
#[test]
fn the_rail_moves_nothing_while_the_deploy_has_it_off() {
    let quote = quote();
    let off = Config {
        eco_enabled: EcoEnabled::OFF,
        ..config()
    };
    assert!(
        !Config::default().eco_enabled.is_on(),
        "and off is what a deploy gets"
    );
    let intent = intent(2_000);
    let fresh = fixture_swap(None, None);
    assert_eq!(
        Eco.step(&at(&quote, &fresh, &off, None, Some(&intent)))
            .map(drop),
        Err(RailError::RailDisabled { rail: Rail::Eco })
    );
    let published = fixture_swap(Some(SwapLeg::Burn), Some(Outcome::Confirmed));
    let mut past = at(&quote, &published, &off, None, Some(&intent));
    past.now = UnixSeconds::new(3_000);
    assert_eq!(
        Eco.reclaim(&past).map(drop),
        Err(RailError::RailDisabled { rail: Rail::Eco }),
        "the reward is locked and the rail is off: the swap waits for the knob"
    );
    // before the deadline too: nothing the rail could do later is done with it off
    let before = at(&quote, &published, &off, None, Some(&intent));
    assert_eq!(
        Eco.reclaim(&before).map(drop),
        Err(RailError::RailDisabled { rail: Rail::Eco })
    );
    // and the knob back on resumes it where it was
    let on = config();
    let mut resumed = at(&quote, &published, &on, None, Some(&intent));
    resumed.now = UnixSeconds::new(3_000);
    assert!(matches!(Eco.reclaim(&resumed), Ok(ReclaimStep::Send(_))));
}
