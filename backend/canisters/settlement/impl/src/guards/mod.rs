use crate::storage::halt::is_halted;
use crate::storage::roles::{self, Roles};
use candid::Principal;

/// The ops rule: controllers only. The two service rules live below.
pub fn require_controller() -> Result<(), String> {
    if ic_cdk::api::is_controller(&ic_cdk::api::caller()) {
        Ok(())
    } else {
        Err("not controller".to_string())
    }
}

/// The shared shape of a role check. It names the role and never the principals: a
/// stranger learns whether the role is configured, nothing about who holds it.
fn check(role: Option<Principal>, caller: Principal, name: &str) -> Result<(), String> {
    match role {
        None => Err(format!("{name} role is not set")),
        Some(p) if p == caller => Ok(()),
        Some(_) => Err(format!("caller is not the {name}")),
    }
}

pub fn require_quoter() -> Result<(), String> {
    check(roles::get().quoter, ic_cdk::api::caller(), "quoter")
}

pub fn require_watcher() -> Result<(), String> {
    check(roles::get().watcher, ic_cdk::api::caller(), "watcher")
}

/// Either service. One error for both, so a refusal says nothing about which role the
/// caller failed to be.
fn check_either(roles: &Roles, caller: Principal) -> Result<(), String> {
    match (roles.quoter, roles.watcher) {
        (None, None) => Err("quoter and watcher roles are not set".to_string()),
        (q, w) if q == Some(caller) || w == Some(caller) => Ok(()),
        _ => Err("caller is not the quoter or the watcher".to_string()),
    }
}

pub fn require_quoter_or_watcher() -> Result<(), String> {
    check_either(&roles::get(), ic_cdk::api::caller())
}

/// The gate Plan 3's money endpoints call before they move anything.
pub fn require_not_halted() -> Result<(), String> {
    if is_halted() {
        Err("canister is halted: a replay audit found the log and the state disagree".to_string())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
