use crate::types::errors::{AppendError, GuardError};
use crate::types::events::{EvmAddressError, Hash32};
use crate::types::evm::EcdsaError;
use candid::{CandidType, Nat};
use serde::Deserialize;

/// Why no transaction was created.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq)]
pub enum TxError {
    Guard(GuardError),
    /// The watcher has pushed nothing young enough to price a transaction with.
    StaleChainData {
        chain_id: u64,
    },
    /// The chain reading prices this transaction above the ceiling this canister will pay
    /// per unit of gas, so nothing was signed.
    FeeOutOfRange {
        chain_id: u64,
        ceiling_wei_per_gas: Nat,
    },
    /// The price and the gas limit together are above the most this canister will spend on
    /// one transaction.
    GasCostTooHigh {
        chain_id: u64,
        cost_wei: Nat,
        bound_wei: Nat,
    },
    /// A transaction is signed against a swap's attempt, and this purpose names no swap.
    PurposeNeedsASwap {
        purpose: String,
    },
    /// The text the caller gave as the transaction's destination is not an address.
    NotAnAddress {
        to: String,
        reason: EvmAddressError,
    },
    /// The swap has used every attempt number there is.
    NoAttemptLeft {
        quote_hash: Hash32,
    },
    Append(AppendError),
    Ecdsa(EcdsaError),
}
