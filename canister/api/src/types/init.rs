use crate::types::config::Config;
use candid::{CandidType, Principal};
use serde::Deserialize;

/// What an install starts from: the whole config and both service roles, so a canister is
/// never live on default knobs or without its quoter and watcher. Each is validated as
/// `set_config` and `set_roles` validate it, and anything invalid fails the install.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq)]
pub struct InitArg {
    pub config: Config,
    pub quoter: Principal,
    pub watcher: Principal,
}
