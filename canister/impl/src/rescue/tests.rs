//! The rescue's plan, on the live cancel of 2026-10-11 (`research/eco-cancel-test`, the data
//! copied into `cancel_test.txt`): a 1 USDC Polygon to Ethereum intent of the research
//! wallet, published with its route's deadline 1791704547.

use super::*;
use crate::pool::arrival::Log;
use alloy_primitives::{Bytes, FixedBytes, U256};
use alloy_sol_types::SolEvent;
use types::BlockNumber;

fn sample(key: &str) -> Vec<u8> {
    let line = include_str!("cancel_test.txt")
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{key}=")))
        .unwrap();
    hex::decode(line.trim()).unwrap()
}

fn address(text: &str) -> EvmAddress {
    text.parse().unwrap()
}

fn prover() -> EvmAddress {
    address("0xEC08fb4647f3f50d1162a578d481266687C60fc5")
}

fn portal() -> EvmAddress {
    address("0xEC000769A73b70e16f361a442292500b3BCf4A85")
}

fn live() -> PublishedIntent {
    PublishedIntent {
        intent_hash: sample("intent_hash").try_into().unwrap(),
        destination: 1,
        creator: address("0x7551A66653f9a20979ed81835a0b7008EC83401b"),
        route: sample("route"),
        prover: prover(),
        reward_deadline: 1_791_791_067,
        reward_native: U256::ZERO,
        reward_tokens: vec![(
            address("0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359"),
            U256::from(1_000_000_u64),
        )],
    }
}

const DEADLINE: u64 = 1_791_704_547;

#[test]
fn an_unfilled_intent_past_its_deadline_is_rescued_with_the_call_that_cancelled_it() {
    let rescue = plan(&live(), Some(prover()), DEADLINE + RESCUE_MARGIN_S + 1).unwrap();
    assert_eq!(rescue.destination, ChainId::ETHEREUM);
    assert_eq!(rescue.intent.to_vec(), sample("intent_hash"));
    assert_eq!(rescue.reward_hash.to_vec(), sample("reward_hash"));
    let call = eco::cancel_and_prove_calldata(
        rescue.intent,
        &rescue.route,
        rescue.reward_hash,
        rescue.prover,
        137,
        &eco::prove_data(rescue.prover),
    )
    .unwrap();
    assert_eq!(call, sample("cancel_and_prove"));
}

#[test]
fn an_intent_still_fillable_of_another_prover_or_a_wrong_hash_is_not_rescued() {
    let early = plan(&live(), Some(prover()), DEADLINE + RESCUE_MARGIN_S);
    assert!(early.unwrap_err().contains("may still be filled"));
    let other = plan(
        &live(),
        Some(address("0x000000000000000000000000000000000000dEaD")),
        DEADLINE + 3_600,
    );
    assert!(other.unwrap_err().contains("another prover"));
    assert!(plan(&live(), None, DEADLINE + 3_600)
        .unwrap_err()
        .contains("no Eco prover"));
    // a log whose hash is not its route's and reward's
    let forged = PublishedIntent {
        reward_deadline: 1_791_791_068,
        ..live()
    };
    assert!(plan(&forged, Some(prover()), DEADLINE + 3_600)
        .unwrap_err()
        .contains("hash"));
}

/// The receipt's `IntentPublished` from Eco's Portal names the intent; one from anyone else
/// names nothing.
#[test]
fn the_intent_is_read_off_the_portals_own_log() {
    let intent = live();
    let event = swapic::IntentPublished {
        intentHash: FixedBytes(intent.intent_hash),
        destination: intent.destination,
        route: Bytes::from(intent.route.clone()),
        creator: swapic::addr(intent.creator),
        prover: swapic::addr(intent.prover),
        rewardDeadline: intent.reward_deadline,
        rewardNativeAmount: intent.reward_native,
        rewardTokens: vec![swapic::EcoTokenAmount {
            token: swapic::addr(intent.reward_tokens[0].0),
            amount: intent.reward_tokens[0].1,
        }],
    };
    let data = event.encode_log_data();
    let log = |emitter: EvmAddress| Log {
        index: 3,
        address: emitter,
        topics: data.topics().iter().map(|topic| topic.0).collect(),
        data: data.data.to_vec(),
        block_time: None,
    };
    let receipt = |emitter| Receipt {
        tx: TxHash::new([1; 32]),
        block: BlockNumber::new(2),
        success: true,
        logs: vec![log(emitter)],
    };
    assert_eq!(published(&receipt(portal()), portal()), Some(intent));
    assert_eq!(
        published(&receipt(address("0x000000000000000000000000000000000000dEaD")), portal()),
        None
    );
}
