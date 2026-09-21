//! The canister-level errors: the guards, the append chokepoint, and each endpoint's
//! composition of them with the domain errors.

use crate::types::chain_data::ChainDataError;
use crate::types::config::ConfigError;
use crate::types::events::{CanonicalError, EventError, Hash32};
use crate::types::quote::QuoteError;
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
    Halted,
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
