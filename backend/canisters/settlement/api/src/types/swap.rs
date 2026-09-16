use candid::{CandidType, Nat};
use serde::Deserialize;

/// Where a swap is in its lifecycle. Plan 3 and the indexer read a swap's progress from it.
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

/// The folded state of one swap. `waiting_since_ns` is IC time in nanoseconds.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq)]
pub struct Swap {
    pub quote_bytes: Vec<u8>,
    pub status: SwapStatus,
    pub last_attempt: Option<u32>,
    pub open_attempt: Option<u32>,
    pub src_chain: u64,
    pub src_token: String,
    pub amount_in: Nat,
    pub amount_paid: Nat,
    pub waiting_since_ns: Option<u64>,
}

impl From<types::SwapStatus> for SwapStatus {
    fn from(status: types::SwapStatus) -> Self {
        use types::SwapStatus as Domain;
        match status {
            Domain::FundsReceived => Self::FundsReceived,
            Domain::Executing => Self::Executing,
            Domain::PaidInStable => Self::PaidInStable,
            Domain::Delivering => Self::Delivering,
            Domain::WaitingForUser => Self::WaitingForUser,
            Domain::Done => Self::Done,
            Domain::Refunding => Self::Refunding,
            Domain::Refunded => Self::Refunded,
            Domain::Frozen => Self::Frozen,
        }
    }
}

impl From<types::Swap> for Swap {
    fn from(swap: types::Swap) -> Self {
        Self {
            quote_bytes: swap.quote_bytes,
            status: swap.status.into(),
            last_attempt: swap.last_attempt.map(|attempt| attempt.get()),
            open_attempt: swap.open_attempt.map(|attempt| attempt.get()),
            src_chain: swap.src_chain.get(),
            src_token: swap.src_token.to_string(),
            amount_in: swap.amount_in.into(),
            amount_paid: swap.amount_paid.into(),
            waiting_since_ns: swap.waiting_since.map(|since| since.as_nanos()),
        }
    }
}
