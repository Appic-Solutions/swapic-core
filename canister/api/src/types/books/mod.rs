//! The canister's USDC books, per chain, in micro-USDC.

use candid::{CandidType, Int, Nat};
use serde::Deserialize;

/// One chain's book lines and its last balance check. `surplus` is `observed` minus what
/// the lines expect at that check; it is never booked by itself.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct BookLines {
    pub fee_income: Nat,
    pub unattributed: Nat,
    pub in_flight: Nat,
    pub pool: Nat,
    pub observed: Option<Nat>,
    pub surplus: Option<Int>,
    pub checked_at_block: Option<u64>,
    /// When the last check that read a balance was recorded, IC time in nanoseconds.
    pub checked_at_ns: Option<u64>,
    /// When the last check that could not read the balance was recorded.
    pub failed_at_ns: Option<u64>,
}

/// One chain's fees, micro USD.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ChainFees {
    pub chain_id: u64,
    /// What the fee income line holds now: withdrawable.
    pub held: Nat,
    /// Of every fee ever booked here, what users paid in transactions of their own. The rest
    /// of `held + swept` was read off the canister's own relays.
    pub user_paid: Nat,
    /// What sweeps took off the fee income line.
    pub swept: Nat,
}

/// Everything the canister has earned, by where it is held, micro USD.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct FeesView {
    pub chains: Vec<ChainFees>,
    /// The pool's fee line on Base: move fees and network fees on what the pool pays out.
    pub pool_base: Nat,
    /// The pool's fee line on ICP, in ckUSDC.
    pub pool_icp: Nat,
    /// What sweeps took off the pool's Base and ICP fee lines.
    pub pool_base_swept: Nat,
    pub pool_icp_swept: Nat,
}
