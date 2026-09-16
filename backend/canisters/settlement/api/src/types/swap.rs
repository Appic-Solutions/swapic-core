use candid::CandidType;
use serde::Deserialize;

// CandidType and Deserialize on the two swap types: `get_swap` answers with a SwapState,
// and the folded status is what Plan 3 and the indexer read a swap's progress from.
#[derive(CandidType, Deserialize, Clone, Copy, Debug, PartialEq)]
pub enum SwapStatus {
    FundsReceived,
    Executing,
    PaidInStable,
    Delivering,
    WaitingForUser,
    Done,
    Refunding,
    Refunded,
    Frozen,
}

impl SwapStatus {
    /// Terminal: no further work is ever scheduled for the swap.
    pub fn is_closed(self) -> bool {
        matches!(self, Self::Done | Self::Refunded | Self::Frozen)
    }
}

#[derive(CandidType, Deserialize, Clone, Debug, PartialEq)]
pub struct SwapState {
    pub quote_bytes: Vec<u8>,
    pub status: SwapStatus,
    pub attempts: u32,
    pub open_attempt: Option<u32>,
    pub src_chain: u64,
    pub src_token: String,
    pub amount_in: u128,
    pub amount_paid: u128,
    pub waiting_since_ns: Option<u64>,
}
