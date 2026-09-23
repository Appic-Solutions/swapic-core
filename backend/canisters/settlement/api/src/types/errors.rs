//! The canister-level errors: the guards, the append chokepoint, and each endpoint's
//! composition of them with the domain errors.

use crate::types::chain_data::ChainDataError;
use crate::types::config::ConfigError;
use crate::types::events::{CanonicalError, EventError, EvmAddressError, Hash32};
use crate::types::evm::EcdsaError;
use crate::types::quote::{QuoteAddressError, QuoteError};
use crate::types::rpc::RpcError;
use crate::types::swap::TransitionError;
use candid::CandidType;
use serde::Deserialize;

/// A service role the canister hands out.
#[derive(CandidType, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Quoter,
    Watcher,
}

/// Why a caller was refused before anything ran. A refusal names the role and never a
/// principal.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum GuardError {
    NotController,
    RoleNotSet(Role),
    CallerNotRole(Role),
    RolesNotSet,
    CallerNotQuoterOrWatcher,
    /// Neither service principal, and not a controller either: the claim's door, which an
    /// operator may open by hand.
    CallerNotQuoterWatcherOrController,
    Halted,
    CallerNotWatcherOrController,
}

/// Why an event was not appended. Nothing was written.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq)]
pub enum AppendError {
    ChainDiverged {
        log_head: Hash32,
        state_head: Hash32,
    },
    IndexMismatch {
        log_len: u64,
        sealed: u64,
    },
    LogFull,
    OutOfStableMemory {
        current_pages: u64,
        delta_pages: u64,
    },
    Transition(TransitionError),
    Canonical(CanonicalError),
}

/// Why `register_quote` stored nothing.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum RegisterQuoteError {
    Guard(GuardError),
    InvalidQuote(QuoteError),
    Expired {
        expires_at_s: u64,
        now_s: u64,
    },
    ExpiresTooFarAhead {
        expires_at_s: u64,
        now_s: u64,
        max_lifetime_s: u64,
    },
    StoreFull {
        capacity: u64,
    },
    /// The quote names no refund address, so no refund could ever be paid on it.
    NoRefundAddress,
    /// The quote's refund address is not an EVM address.
    RefundAddressNotAnAddress {
        reason: EvmAddressError,
    },
    /// The quote's destination address is not an address the destination chain can be
    /// paid at: every chain this canister pays is an EVM chain, so not an EVM address.
    DstAddressNotAnAddress {
        reason: EvmAddressError,
    },
    /// The quote names a rail this deploy does not run, by its id: its claim would be
    /// refused, so the quote is not handed to a user to pay.
    RailUnavailable {
        rail: String,
    },
    /// A payee the quote names is one the vault cannot pay: the zero address as the
    /// destination or the refund address.
    QuoteAddress(QuoteAddressError),
}

/// Why `push_chain_data` stored nothing.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum PushChainDataError {
    Guard(GuardError),
    InvalidData(ChainDataError),
}

/// Why `set_config` wrote nothing.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq)]
pub enum SetConfigError {
    Guard(GuardError),
    InvalidConfig(ConfigError),
    Append(AppendError),
}

/// Why `set_roles` wrote nothing.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq)]
pub enum SetRolesError {
    Guard(GuardError),
    AnonymousRole(Role),
    Append(AppendError),
}

/// Why a call that needs the canister's threshold key answered nothing.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum SignError {
    Guard(GuardError),
    Ecdsa(EcdsaError),
}

/// Why `set_sanctioned` changed nothing.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum SetSanctionedError {
    Guard(GuardError),
    /// The text at `index` of `list` (`"add"` or `"remove"`) is above the 256-byte cap.
    TextTooLong {
        list: String,
        index: u64,
        len: u64,
    },
    /// The set would grow past its cap; nothing of the call landed.
    SetFull {
        capacity: u64,
    },
}

/// Why the test-only `test_rpc_batch` read nothing.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum TestRpcError {
    Guard(GuardError),
    Rpc(RpcError),
}

/// Why the test-only `test_append` wrote nothing.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq)]
pub enum TestAppendError {
    Guard(GuardError),
    InvalidEvent(EventError),
    Append(AppendError),
}
