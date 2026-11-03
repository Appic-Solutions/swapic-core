//! The canister-level errors: the guards, the append chokepoint, and each endpoint's
//! composition of them with the domain errors.

use crate::types::config::ConfigError;
use crate::types::events::EventError;
use crate::types::evm::EcdsaError;
use crate::types::rpc::RpcError;
use crate::types::transition::TransitionError;
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
    /// Neither service principal, and not a controller either: the claim's door, which ops
    /// may open by hand.
    CallerNotQuoterWatcherOrController,
    Halted,
    CallerNotWatcherOrController,
}

/// Why an event was not appended. Nothing was written.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq)]
pub enum AppendError {
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
