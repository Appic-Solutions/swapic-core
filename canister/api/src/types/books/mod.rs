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
}
