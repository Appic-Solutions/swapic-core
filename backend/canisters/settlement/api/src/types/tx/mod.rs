use crate::types::errors::{AppendError, GuardError};
use crate::types::events::Hash32;
use crate::types::evm::EcdsaError;
use candid::CandidType;
use serde::Deserialize;

/// Why no transaction was created.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq)]
pub enum TxError {
    Guard(GuardError),
    /// The watcher has pushed nothing young enough to price a transaction with.
    StaleChainData {
        chain_id: u64,
    },
    FeeOutOfRange {
        chain_id: u64,
    },
    /// A transaction is signed against a swap's attempt, and this purpose names no swap.
    PurposeNeedsASwap {
        purpose: String,
    },
    /// The swap has used every attempt number there is.
    NoAttemptLeft {
        quote_hash: Hash32,
    },
    Append(AppendError),
    Ecdsa(EcdsaError),
}
