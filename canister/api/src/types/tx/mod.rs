use crate::types::errors::{AppendError, GuardError};
use crate::types::events::{EvmAddressError, Hash32};
use crate::types::evm::EcdsaError;
use candid::{CandidType, Nat};
use serde::Deserialize;

/// Why no transaction was created.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq)]
pub enum TxError {
    Guard(GuardError),
    /// No reading young enough to price a transaction with: none came with the call, and
    /// the canister's own read of the chain failed.
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
    /// The chain has no gas budget for this action, so nothing is signed for it.
    NoGasBudget {
        chain_id: u64,
        action: String,
    },
    /// The price and the gas limit together are above the gas budget of this action.
    OverGasBudget {
        chain_id: u64,
        action: String,
        cost_wei: Nat,
        budget_wei: Nat,
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
    /// The nonce the signed bytes carry was cancelled while the signature was on its way,
    /// so the record was refused and nothing was queued.
    NonceCancelled {
        quote_hash: Hash32,
        chain_id: u64,
        nonce: u64,
    },
    Append(AppendError),
    Ecdsa(EcdsaError),
}

/// The chain a signed transaction is for.
#[derive(CandidType, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChainRef {
    Evm(u64),
    Solana,
}

/// A transaction the canister signed, for the watcher to broadcast. `id` orders them and
/// is the cursor `signed_txs` pages by.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SignedTx {
    pub id: u64,
    pub chain: ChainRef,
    pub raw: Vec<u8>,
    pub hash: Vec<u8>,
    pub signed_at_ns: u64,
}
