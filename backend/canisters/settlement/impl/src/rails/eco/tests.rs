use super::*;
use crate::rails::tests::{config, fixture_swap, leg, quote, USDC_BASE, VAULT_BASE};
use crate::rails::{RailStep, WaitingFor};
use types::abi::{decode_eco_publish_and_fund, decode_vault_execute};
use types::{ChainId, Config, EvmAddress, Outcome, TokenAmount, UnixSeconds};

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
    let leg = leg(&quote, &swap, &config, None, Some(&intent));
    let publish = Eco
        .publish(&leg, &intent)
        .expect("everything is configured");
    assert_eq!(publish.purpose, TxPurpose::Burn(leg.quote_hash));
    assert_eq!(publish.chain_id, ChainId::BASE);
    assert_eq!(publish.to, VAULT_BASE.parse::<EvmAddress>().unwrap());
    assert_eq!(publish.gas_limit, PUBLISH_GAS_LIMIT);
    let (swap_ref, calls, deltas) = decode_vault_execute(&publish.data).expect("an execute");
    assert_eq!(swap_ref, leg.quote_hash);
    let usdc_base: EvmAddress = USDC_BASE.parse().unwrap();
    assert_eq!(calls[0].target, config.eco_portal.unwrap());
    assert_eq!(calls[0].approve_token, usdc_base);
    assert_eq!(calls[0].approve_amount, swap.amount_in);
    let (destination, route, reward) =
        decode_eco_publish_and_fund(&calls[0].data).expect("a publishAndFund");
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
        Eco.step(&leg(&quote, &fresh, &config, None, None)),
        Ok(RailStep::Wait(WaitingFor::Intent))
    );
    let intent = intent(2_000);
    assert!(matches!(
        Eco.step(&leg(&quote, &fresh, &config, None, Some(&intent))),
        Ok(RailStep::Send(RailTx {
            purpose: TxPurpose::Burn(_),
            ..
        }))
    ));

    let published = fixture_swap(Some(SwapLeg::Burn), Some(Outcome::Confirmed));
    let mut before = leg(&quote, &published, &config, None, Some(&intent));
    before.now = UnixSeconds::new(2_000);
    assert_eq!(
        Eco.step(&before),
        Ok(RailStep::CheckArrival {
            chain_id: ChainId::ARBITRUM,
            expired: false
        })
    );
    let mut after = leg(&quote, &published, &config, None, Some(&intent));
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
        Ok(RailStep::Wait(WaitingFor::Deadline)),
        "the reward is the filler's until the deadline"
    );
    let reclaim = match Eco.reclaim(&after) {
        Ok(RailStep::Reclaim(tx)) => tx,
        other => panic!("past the deadline the reward is reclaimed: {other:?}"),
    };
    assert_eq!(reclaim.purpose, TxPurpose::Reclaim(after.quote_hash));
    assert_eq!(reclaim.chain_id, ChainId::BASE);
    assert_eq!(reclaim.to, config.eco_portal.unwrap());
    assert_eq!(reclaim.gas_limit, RECLAIM_GAS_LIMIT);
    assert_eq!(
        &reclaim.data[..4],
        &[0x30, 0x8a, 0xda, 0xde],
        "refund(uint64,bytes32,Reward)"
    );
    assert_eq!(
        &reclaim.data[36..68],
        &eco_route_hash(&[0xde, 0xad, 0xbe, 0xef]),
        "the route by its hash"
    );
    let without_intent = leg(&quote, &published, &config, None, None);
    assert!(matches!(
        Eco.reclaim(&without_intent),
        Ok(RailStep::Stuck(_))
    ));

    let no_portal = Config {
        eco_portal: None,
        ..config.clone()
    };
    assert_eq!(
        Eco.step(&leg(&quote, &fresh, &no_portal, None, Some(&intent)))
            .map(drop),
        Err(RailError::NoEcoPortal)
    );
}
