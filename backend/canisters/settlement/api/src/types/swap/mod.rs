use crate::types::events::Hash32;
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
    /// Absent until the swap is paid in stable.
    pub amount_paid: Option<Nat>,
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
            amount_paid: swap.amount_paid.map(Nat::from),
            waiting_since_ns: swap.waiting_since.map(|since| since.as_nanos()),
        }
    }
}

/// Why an event cannot move the state it was offered to.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq)]
pub enum TransitionError {
    SwapExists(Hash32),
    UnknownSwap(Hash32),
    UnknownPocket(u64),
    SwapClosed(SwapStatus),
    WaitingForUser,
    NotWaitingForUser(SwapStatus),
    AttemptStillOpen(u32),
    AttemptOutOfSequence { attempt: u32, expected: Option<u32> },
    AttemptNotOpen(u32),
    NotExecuting(SwapStatus),
    AlreadyPaid,
    CannotStartRefund(SwapStatus),
    NotRefunding(SwapStatus),
    NotInFlight(SwapStatus),
    FeesOverflow,
    AmountOutOfRange(Nat),
    Pocket(PocketError),
}

/// Why a pocket cannot make a move.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq)]
pub enum PocketError {
    InsufficientAvailable { available: Nat, requested: Nat },
    InsufficientReserved { reserved: Nat, requested: Nat },
    Overflow,
}

impl From<types::TransitionError> for TransitionError {
    fn from(error: types::TransitionError) -> Self {
        use types::TransitionError as Domain;
        match error {
            Domain::SwapExists(quote_hash) => Self::SwapExists(quote_hash.into_bytes()),
            Domain::UnknownSwap(quote_hash) => Self::UnknownSwap(quote_hash.into_bytes()),
            Domain::UnknownPocket(chain_id) => Self::UnknownPocket(chain_id.get()),
            Domain::SwapClosed(status) => Self::SwapClosed(status.into()),
            Domain::WaitingForUser => Self::WaitingForUser,
            Domain::NotWaitingForUser(status) => Self::NotWaitingForUser(status.into()),
            Domain::AttemptStillOpen(attempt) => Self::AttemptStillOpen(attempt.get()),
            Domain::AttemptOutOfSequence { attempt, expected } => Self::AttemptOutOfSequence {
                attempt: attempt.get(),
                expected: expected.map(|attempt| attempt.get()),
            },
            Domain::AttemptNotOpen(attempt) => Self::AttemptNotOpen(attempt.get()),
            Domain::NotExecuting(status) => Self::NotExecuting(status.into()),
            Domain::AlreadyPaid => Self::AlreadyPaid,
            Domain::CannotStartRefund(status) => Self::CannotStartRefund(status.into()),
            Domain::NotRefunding(status) => Self::NotRefunding(status.into()),
            Domain::NotInFlight(status) => Self::NotInFlight(status.into()),
            Domain::FeesOverflow => Self::FeesOverflow,
            Domain::AmountOutOfRange(amount) => Self::AmountOutOfRange(amount.into()),
            Domain::Pocket(error) => Self::Pocket(error.into()),
        }
    }
}

impl From<types::PocketError> for PocketError {
    fn from(error: types::PocketError) -> Self {
        use types::PocketError as Domain;
        match error {
            Domain::InsufficientAvailable {
                available,
                requested,
            } => Self::InsufficientAvailable {
                available: available.into(),
                requested: requested.into(),
            },
            Domain::InsufficientReserved {
                reserved,
                requested,
            } => Self::InsufficientReserved {
                reserved: reserved.into(),
                requested: requested.into(),
            },
            Domain::Overflow => Self::Overflow,
        }
    }
}

#[cfg(test)]
mod tests;
