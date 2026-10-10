//! The endpoint-edge checks. They answer in the wire's own `GuardError`, since a refusal
//! is only ever returned to the caller, and a refusal names the role and never a
//! principal: a stranger learns whether a role is configured, nothing about who holds it.

use crate::storage::halt::is_halted;
use crate::storage::roles::{self, Roles};
use candid::Principal;
use settlement_api::types::errors::{GuardError, Role};

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

/// Either service, or a controller standing in for one: the claim is the door an operator
/// takes when a deposit has to be claimed by hand, so the controller may open it too. One
/// error for all three, so a refusal says nothing about which of them the caller failed to
/// be.
fn check_either_or_controller(
    roles: &Roles,
    caller: Principal,
    is_controller: bool,
) -> Result<(), GuardError> {
    if is_controller {
        return Ok(());
    }
    check_either(roles, caller).map_err(|_| GuardError::CallerNotQuoterWatcherOrController)
}

pub fn require_quoter_watcher_or_controller() -> Result<(), GuardError> {
    let caller = ic_cdk::api::caller();
    check_either_or_controller(&roles::get(), caller, ic_cdk::api::is_controller(&caller))
}

/// The watcher, or a controller standing in for it: the compliance data the watcher pushes
/// is also what an operator corrects by hand. One error for both, so a refusal says nothing
/// about which of the two the caller failed to be.
fn check_watcher_or_controller(
    watcher: Option<Principal>,
    caller: Principal,
    is_controller: bool,
) -> Result<(), GuardError> {
    if is_controller || watcher == Some(caller) {
        Ok(())
    } else {
        Err(GuardError::CallerNotWatcherOrController)
    }
}

pub fn require_watcher_or_controller() -> Result<(), GuardError> {
    let caller = ic_cdk::api::caller();
    check_watcher_or_controller(
        roles::get().watcher,
        caller,
        ic_cdk::api::is_controller(&caller),
    )
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
