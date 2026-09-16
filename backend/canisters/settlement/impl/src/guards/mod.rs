use crate::storage::halt::is_halted;
use crate::storage::roles::{self, Role, Roles};
use candid::Principal;
use thiserror::Error;

/// Why a caller was refused before anything ran. A refusal names the role and never a
/// principal: a stranger learns whether a role is configured, nothing about who holds it.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum GuardError {
    #[error("caller is not a controller")]
    NotController,
    #[error("{0} role is not set")]
    RoleNotSet(Role),
    #[error("caller is not the {0}")]
    CallerNotRole(Role),
    #[error("quoter and watcher roles are not set")]
    RolesNotSet,
    #[error("caller is not the quoter or the watcher")]
    CallerNotQuoterOrWatcher,
    #[error("canister is halted: a replay audit found the log and the state disagree")]
    Halted,
}

impl From<GuardError> for settlement_api::types::errors::GuardError {
    fn from(error: GuardError) -> Self {
        match error {
            GuardError::NotController => Self::NotController,
            GuardError::RoleNotSet(role) => Self::RoleNotSet(role.into()),
            GuardError::CallerNotRole(role) => Self::CallerNotRole(role.into()),
            GuardError::RolesNotSet => Self::RolesNotSet,
            GuardError::CallerNotQuoterOrWatcher => Self::CallerNotQuoterOrWatcher,
            GuardError::Halted => Self::Halted,
        }
    }
}

/// The ops rule: controllers only. The two service rules live below.
pub fn require_controller() -> Result<(), GuardError> {
    if ic_cdk::api::is_controller(&ic_cdk::api::caller()) {
        Ok(())
    } else {
        Err(GuardError::NotController)
    }
}

/// The shared shape of a role check.
fn check(holder: Option<Principal>, caller: Principal, role: Role) -> Result<(), GuardError> {
    match holder {
        None => Err(GuardError::RoleNotSet(role)),
        Some(holder) if holder == caller => Ok(()),
        Some(_) => Err(GuardError::CallerNotRole(role)),
    }
}

pub fn require_quoter() -> Result<(), GuardError> {
    check(roles::get().quoter, ic_cdk::api::caller(), Role::Quoter)
}

pub fn require_watcher() -> Result<(), GuardError> {
    check(roles::get().watcher, ic_cdk::api::caller(), Role::Watcher)
}

/// Either service. One error for both, so a refusal says nothing about which role the
/// caller failed to be.
fn check_either(roles: &Roles, caller: Principal) -> Result<(), GuardError> {
    match (roles.quoter, roles.watcher) {
        (None, None) => Err(GuardError::RolesNotSet),
        (q, w) if q == Some(caller) || w == Some(caller) => Ok(()),
        _ => Err(GuardError::CallerNotQuoterOrWatcher),
    }
}

pub fn require_quoter_or_watcher() -> Result<(), GuardError> {
    check_either(&roles::get(), ic_cdk::api::caller())
}

/// The gate Plan 3's money endpoints call before they move anything.
pub fn require_not_halted() -> Result<(), GuardError> {
    if is_halted() {
        Err(GuardError::Halted)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
