use crate::types::events::Hash32;
use crate::types::quote::QuoteError;
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
    AttemptOutOfSequence {
        attempt: u32,
        expected: Option<u32>,
    },
    AttemptNotOpen(u32),
    NoOpenAttempt(Hash32),
    NonceOutOfSequence {
        chain_id: u64,
        nonce: u64,
        expected: u64,
    },
    NonceNeverAllocated {
        chain_id: u64,
        nonce: u64,
        next: u64,
    },
    NonceExhausted {
        chain_id: u64,
    },
    NonceStillUnsigned(Hash32),
    NonceNotUnsigned {
        chain_id: u64,
        nonce: u64,
    },
    NoUnsignedNonce {
        quote_hash: Hash32,
        chain_id: u64,
    },
    NonceNotHeldForPull {
        chain_id: u64,
        nonce: u64,
        quote_hash: Hash32,
    },
    NotExecuting(SwapStatus),
    AlreadyPaid,
    CannotStartRefund(SwapStatus),
    CannotAskWhileRefunding,
    NotRefunding(SwapStatus),
    NotInFlight(SwapStatus),
    FeesOverflow,
    AmountOutOfRange(Nat),
    UnparseableQuote(QuoteError),
    QuoteHashMismatch {
        declared: Hash32,
        computed: Hash32,
    },
    Pocket(PocketError),
    /// The line says the funds arrived on `logged`, and the quote it carries is for
    /// `quoted`.
    FundsChainNotTheQuotes {
        logged: u64,
        quoted: u64,
    },
    /// The line says `logged` arrived, and the quote it carries is for `quoted`.
    FundsTokenNotTheQuotes {
        logged: String,
        quoted: String,
    },
    /// The line says `logged` arrived, and the quote it carries is for `quoted`.
    FundsAmountNotTheQuotes {
        logged: Nat,
        quoted: Nat,
    },
    /// The swap already holds its platform fee: one fee line per swap.
    FeeAlreadyAccrued(Hash32),
    /// A flow step refused, with the domain's reason.
    Flow(String),
    Books(String),
    Pool(String),
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
            Domain::NoOpenAttempt(quote_hash) => Self::NoOpenAttempt(quote_hash.into_bytes()),
            Domain::NonceOutOfSequence {
                chain_id,
                nonce,
                expected,
            } => Self::NonceOutOfSequence {
                chain_id: chain_id.get(),
                nonce: nonce.get(),
                expected: expected.get(),
            },
            Domain::NonceNeverAllocated {
                chain_id,
                nonce,
                next,
            } => Self::NonceNeverAllocated {
                chain_id: chain_id.get(),
                nonce: nonce.get(),
                next: next.get(),
            },
            Domain::NonceExhausted { chain_id } => Self::NonceExhausted {
                chain_id: chain_id.get(),
            },
            Domain::NonceStillUnsigned(quote_hash) => {
                Self::NonceStillUnsigned(quote_hash.into_bytes())
            }
            Domain::NonceNotUnsigned { chain_id, nonce } => Self::NonceNotUnsigned {
                chain_id: chain_id.get(),
                nonce: nonce.get(),
            },
            Domain::NoUnsignedNonce {
                quote_hash,
                chain_id,
            } => Self::NoUnsignedNonce {
                quote_hash: quote_hash.into_bytes(),
                chain_id: chain_id.get(),
            },
            Domain::NonceNotHeldForPull {
                chain_id,
                nonce,
                quote_hash,
            } => Self::NonceNotHeldForPull {
                chain_id: chain_id.get(),
                nonce: nonce.get(),
                quote_hash: quote_hash.into_bytes(),
            },
            Domain::NotExecuting(status) => Self::NotExecuting(status.into()),
            Domain::AlreadyPaid => Self::AlreadyPaid,
            Domain::CannotStartRefund(status) => Self::CannotStartRefund(status.into()),
            Domain::CannotAskWhileRefunding => Self::CannotAskWhileRefunding,
            Domain::NotRefunding(status) => Self::NotRefunding(status.into()),
            Domain::NotInFlight(status) => Self::NotInFlight(status.into()),
            Domain::FeesOverflow => Self::FeesOverflow,
            Domain::AmountOutOfRange(amount) => Self::AmountOutOfRange(amount.into()),
            Domain::UnparseableQuote(error) => Self::UnparseableQuote(error.into()),
            Domain::QuoteHashMismatch { declared, computed } => Self::QuoteHashMismatch {
                declared: declared.into_bytes(),
                computed: computed.into_bytes(),
            },
            Domain::Pocket(error) => Self::Pocket(error.into()),
            Domain::FundsChainNotTheQuotes { logged, quoted } => Self::FundsChainNotTheQuotes {
                logged: logged.get(),
                quoted: quoted.get(),
            },
            Domain::FundsTokenNotTheQuotes { logged, quoted } => Self::FundsTokenNotTheQuotes {
                logged: logged.to_string(),
                quoted: quoted.to_string(),
            },
            Domain::FundsAmountNotTheQuotes { logged, quoted } => Self::FundsAmountNotTheQuotes {
                logged: logged.into(),
                quoted: quoted.into(),
            },
            Domain::FeeAlreadyAccrued(quote_hash) => {
                Self::FeeAlreadyAccrued(quote_hash.into_bytes())
            }
            Domain::Flow(error) => Self::Flow(error.to_string()),
            Domain::Books(error) => Self::Books(error.to_string()),
            Domain::Pool(error) => Self::Pool(error.to_string()),
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
