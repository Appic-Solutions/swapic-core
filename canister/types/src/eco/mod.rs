//! Eco's Portal as the canister reads and calls it (P11e): whether an intent was filled on its
//! destination (`claimants`), whether its reward was refunded on its source
//! (`getRewardStatus`), and the rescue of an unfilled one (`cancelAndProve` on the
//! destination, which proves the cancellation to the source, where Eco refunds the reward's
//! creator by itself). The hashes are Eco Portal 2.12's: an intent is `keccak256(destination
//! ‖ keccak256(route) ‖ keccak256(reward))`, pinned against a live cancel (2026-10-11,
//! `research/eco-cancel-test`).
//!
//! Pure: no canister calls.

#[cfg(test)]
mod tests;

use crate::evm::EvmAddress;
use crate::swapic::{addr, EcoReward, EcoRoute, EcoTokenAmount};
use alloy_primitives::{keccak256, Bytes, FixedBytes, U256};
use alloy_sol_types::SolValue;

/// `claimants(bytes32)`.
pub const CLAIMANTS: [u8; 4] = [0xed, 0x60, 0xf2, 0xa3];
/// `getRewardStatus(bytes32)`.
pub const GET_REWARD_STATUS: [u8; 4] = [0x12, 0x99, 0xd6, 0x17];
/// `fetchFee(uint64,bytes,bytes)`, on the prover.
pub const FETCH_FEE: [u8; 4] = [0x54, 0xf1, 0x0f, 0x0e];
/// `cancelAndProve(bytes32,(bytes32,uint64,address,uint256,(address,uint256)[],
/// (address,bytes,uint256)[]),bytes32,address,uint64,bytes)`.
pub const CANCEL_AND_PROVE: [u8; 4] = [0xee, 0x43, 0x32, 0x64];

/// The claimant the destination Portal records for a cancelled intent: the address it marks
/// cancellations with, read off the live cancel.
pub const CANCELLED: [u8; 32] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xe6, 0x85, 0x05, 0x6a, 0xec, 0x77, 0x68, 0x6a, 0x83, 0xe2,
    0xa6, 0xbd, 0xf3, 0x7c, 0x6f, 0x71, 0xdd, 0x2f, 0xdb, 0x5f,
];

/// What the destination Portal says of an intent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fill {
    /// No claimant: neither filled nor cancelled.
    Open,
    /// Cancelled: never to be filled.
    Cancelled,
    /// A solver filled it.
    Filled,
}

/// The fill a `claimants` answer reads as.
pub fn fill_of(claimant: [u8; 32]) -> Fill {
    if claimant == [0; 32] {
        Fill::Open
    } else if claimant == CANCELLED {
        Fill::Cancelled
    } else {
        Fill::Filled
    }
}

/// `getRewardStatus`'s answer: 0 initial, 1 funded, 2 withdrawn (the solver paid), 3
/// refunded (the creator paid back).
pub const REWARD_REFUNDED: u8 = 3;
pub const REWARD_WITHDRAWN: u8 = 2;

fn word_call(selector: [u8; 4], intent: [u8; 32]) -> Vec<u8> {
    let mut data = selector.to_vec();
    data.extend_from_slice(&intent);
    data
}

/// `claimants(intent)`, on the destination Portal.
pub fn claimants_calldata(intent: [u8; 32]) -> Vec<u8> {
    word_call(CLAIMANTS, intent)
}

/// `getRewardStatus(intent)`, on the source Portal.
pub fn reward_status_calldata(intent: [u8; 32]) -> Vec<u8> {
    word_call(GET_REWARD_STATUS, intent)
}

/// An intent's reward, as published.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reward {
    pub deadline: u64,
    pub creator: EvmAddress,
    pub prover: EvmAddress,
    pub native: U256,
    pub tokens: Vec<(EvmAddress, U256)>,
}

/// `keccak256(abi.encode(reward))`.
pub fn reward_hash(reward: &Reward) -> [u8; 32] {
    keccak256(SolValue::abi_encode(&EcoReward {
        deadline: reward.deadline,
        creator: addr(reward.creator),
        prover: addr(reward.prover),
        nativeAmount: reward.native,
        tokens: reward
            .tokens
            .iter()
            .map(|(token, amount)| EcoTokenAmount {
                token: addr(*token),
                amount: *amount,
            })
            .collect(),
    }))
    .0
}

/// The intent's hash: `keccak256(destination ‖ keccak256(route) ‖ reward_hash)`, the
/// destination as 8 bytes.
pub fn intent_hash(destination: u64, route: &[u8], reward_hash: [u8; 32]) -> [u8; 32] {
    let mut preimage = destination.to_be_bytes().to_vec();
    preimage.extend_from_slice(&keccak256(route).0);
    preimage.extend_from_slice(&reward_hash);
    keccak256(preimage).0
}

/// The route's own fill deadline, None for bytes that are no route.
pub fn route_deadline(route: &[u8]) -> Option<u64> {
    <EcoRoute as SolValue>::abi_decode(route)
        .ok()
        .map(|route| route.deadline)
}

/// The HyperProver's data for a proof: the source chain's prover (the same one), no
/// metadata, no hook.
pub fn prove_data(prover: EvmAddress) -> Vec<u8> {
    SolValue::abi_encode(&(
        FixedBytes(prover.to_word()),
        Bytes::new(),
        alloy_primitives::Address::ZERO,
    ))
}

/// The proof a cancellation sends to the source: the destination chain, the intent and the
/// cancelled claimant.
pub fn cancel_proofs(destination: u64, intent: [u8; 32]) -> Vec<u8> {
    let mut proofs = destination.to_be_bytes().to_vec();
    proofs.extend_from_slice(&intent);
    proofs.extend_from_slice(&CANCELLED);
    proofs
}

/// `fetchFee(source_domain, proofs, data)`, on the prover: what the proof costs.
pub fn fetch_fee_calldata(source_domain: u64, proofs: &[u8], data: &[u8]) -> Vec<u8> {
    let mut call = FETCH_FEE.to_vec();
    call.extend(SolValue::abi_encode_params(&(
        source_domain,
        Bytes::copy_from_slice(proofs),
        Bytes::copy_from_slice(data),
    )));
    call
}

/// `cancelAndProve(intent, route, reward_hash, prover, source_domain, data)`, on the
/// destination Portal. None for route bytes that are no route.
pub fn cancel_and_prove_calldata(
    intent: [u8; 32],
    route: &[u8],
    reward_hash: [u8; 32],
    prover: EvmAddress,
    source_domain: u64,
    data: &[u8],
) -> Option<Vec<u8>> {
    let route = <EcoRoute as SolValue>::abi_decode(route).ok()?;
    let mut call = CANCEL_AND_PROVE.to_vec();
    call.extend(SolValue::abi_encode_params(&(
        FixedBytes(intent),
        route,
        FixedBytes(reward_hash),
        addr(prover),
        source_domain,
        Bytes::copy_from_slice(data),
    )));
    Some(call)
}
