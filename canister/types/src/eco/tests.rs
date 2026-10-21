//! Pinned against the live cancel of 2026-10-11 (`research/eco-cancel-test`, copied into
//! `samples/cancel_test.txt`): a 1 USDC Polygon to Ethereum intent of the research wallet,
//! its hashes and its `cancelAndProve` call as `cast` built them, and the call that cancelled
//! it on Ethereum (tx `0x9ae6f210...1f77`).

use super::*;

fn sample(key: &str) -> Vec<u8> {
    let line = include_str!("samples/cancel_test.txt")
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{key}=")))
        .unwrap();
    hex::decode(line.trim()).unwrap()
}

fn address(text: &str) -> EvmAddress {
    text.parse().unwrap()
}

fn reward() -> Reward {
    Reward {
        deadline: 1_791_791_067,
        creator: address("0x7551A66653f9a20979ed81835a0b7008EC83401b"),
        prover: address("0xEC08fb4647f3f50d1162a578d481266687C60fc5"),
        native: U256::ZERO,
        tokens: vec![(
            address("0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359"),
            U256::from(1_000_000_u64),
        )],
    }
}

#[test]
fn the_hashes_are_eco_portals() {
    let route = sample("route");
    assert_eq!(reward_hash(&reward()).to_vec(), sample("reward_hash"));
    let intent = intent_hash(1, &route, reward_hash(&reward()));
    assert_eq!(intent.to_vec(), sample("intent_hash"));
    assert_eq!(route_deadline(&route), Some(1_791_704_547));
}

#[test]
fn the_rescue_is_the_call_that_cancelled_the_live_intent() {
    let prover = address("0xEC08fb4647f3f50d1162a578d481266687C60fc5");
    assert_eq!(prove_data(prover), sample("prove_data"));
    let intent: [u8; 32] = sample("intent_hash").try_into().unwrap();
    let call = cancel_and_prove_calldata(
        intent,
        &sample("route"),
        reward_hash(&reward()),
        prover,
        137,
        &prove_data(prover),
    )
    .unwrap();
    assert_eq!(call, sample("cancel_and_prove"));
    // the proof it sends: Ethereum, the intent, the cancelled claimant
    let proofs = cancel_proofs(1, intent);
    assert_eq!(proofs.len(), 8 + 32 + 32);
    assert_eq!(proofs[..8], 1_u64.to_be_bytes());
    assert_eq!(proofs[40..], CANCELLED);
    assert_eq!(fetch_fee_calldata(137, &proofs, &prove_data(prover))[..4], FETCH_FEE);
}

#[test]
fn a_claimant_reads_as_open_cancelled_or_filled() {
    assert_eq!(fill_of([0; 32]), Fill::Open);
    assert_eq!(fill_of(CANCELLED), Fill::Cancelled);
    let mut solver = [0; 32];
    solver[12..].copy_from_slice(&[0x1d; 20]);
    assert_eq!(fill_of(solver), Fill::Filled);
    assert_eq!(claimants_calldata([7; 32])[..4], CLAIMANTS);
    assert_eq!(reward_status_calldata([7; 32]).len(), 36);
}
