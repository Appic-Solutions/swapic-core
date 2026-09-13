use crate::log::{self, Memory, AUTH_MEMORY};
use candid::{CandidType, Principal};
use ic_stable_structures::StableCell;
use serde::Deserialize;
use std::cell::RefCell;

/// The two service principals the canister answers to: the quoter opens quotes, the
/// watcher reads them. Both are unset until a controller calls `set_roles`, and every
/// check below refuses while they are.
#[derive(CandidType, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Roles {
    pub quoter: Option<Principal>,
    pub watcher: Option<Principal>,
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

fn encode(roles: &Roles) -> Vec<u8> {
    candid::encode_one(roles).expect("roles encode")
}

thread_local! {
    // Same two-copy shape as the config: STORED is the record, ROLES is the cheap read.
    static ROLES: RefCell<Roles> = RefCell::new(Roles::default());

    static STORED: RefCell<StableCell<Vec<u8>, Memory>> = RefCell::new(
        StableCell::init(log::memory(AUTH_MEMORY), encode(&Roles::default()))
            .expect("roles cell init"),
    );
}

/// Called from `init` and `post_upgrade`: roles are deploy-time truth, not a fold of the
/// log. Update contexts only, because the first touch of the cell grows stable memory.
pub fn load() {
    // roles that no longer decode trap the upgrade, which leaves the canister on its
    // working wasm; falling back to "unset" would quietly lock both services out
    let stored: Roles = candid::decode_one(STORED.with(|s| s.borrow().get().clone()).as_slice())
        .expect("roles decode");
    ROLES.with(|r| *r.borrow_mut() = stored);
}

pub fn get() -> Roles {
    ROLES.with(|r| r.borrow().clone())
}

/// Writes both copies. The caller does the authorization; this is the storage path.
/// Not logged: roles are deploy-time truth held in the cell, like the config, and the
/// event enum is a hash-chained, cross-repo contract that this task does not extend.
pub fn set_roles(quoter: Principal, watcher: Principal) -> Result<(), String> {
    // the anonymous principal is every unauthenticated caller at once, so a role held by
    // it is a role held by the world
    for (name, p) in [("quoter", quoter), ("watcher", watcher)] {
        if p == Principal::anonymous() {
            return Err(format!("{name} cannot be the anonymous principal"));
        }
    }
    let roles = Roles {
        quoter: Some(quoter),
        watcher: Some(watcher),
    };
    // out of stable memory is not a caller error, so it traps instead of returning Err
    STORED.with(|s| {
        s.borrow_mut()
            .set(encode(&roles))
            .expect("roles cell write")
    });
    ROLES.with(|r| *r.borrow_mut() = roles);
    Ok(())
}

pub fn require_quoter() -> Result<(), String> {
    check(get().quoter, ic_cdk::api::caller(), "quoter")
}

pub fn require_watcher() -> Result<(), String> {
    check(get().watcher, ic_cdk::api::caller(), "watcher")
}

/// Either service. One error for both, so a refusal says nothing about which role the
/// caller failed to be.
pub fn require_quoter_or_watcher() -> Result<(), String> {
    let roles = get();
    let caller = ic_cdk::api::caller();
    match (roles.quoter, roles.watcher) {
        (None, None) => Err("quoter and watcher roles are not set".to_string()),
        (q, w) if q == Some(caller) || w == Some(caller) => Ok(()),
        _ => Err("caller is not the quoter or the watcher".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(b: u8) -> Principal {
        Principal::from_slice(&[b; 29])
    }

    /// The rule the `require_*` wrappers are made of. They read the caller from the
    /// system, which only exists inside a canister, so the rule is tested here instead.
    #[test]
    fn check_refuses_when_unset_and_when_the_caller_is_wrong() {
        assert_eq!(check(Some(p(2)), p(2), "quoter"), Ok(()));

        let unset = check(None, p(2), "quoter").expect_err("an unset role refuses everyone");
        assert!(unset.contains("not set"), "{unset}");

        let wrong = check(Some(p(2)), p(9), "quoter").expect_err("the wrong caller refuses");
        assert!(wrong.contains("quoter"), "{wrong}");
    }

    /// An error a stranger reads must not name the principal that would have worked.
    #[test]
    fn a_refusal_names_the_role_and_never_a_principal() {
        let holder = p(2);
        let err = check(Some(holder), p(9), "quoter").unwrap_err();
        assert!(!err.contains(&holder.to_text()), "leaked the holder: {err}");
        assert!(!err.contains(&p(9).to_text()), "echoed the caller: {err}");
    }
}
