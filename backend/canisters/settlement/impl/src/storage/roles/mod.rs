use crate::storage::events;
use crate::storage::memory::{roles_memory, Memory};
use candid::{CandidType, Principal};
use ic_stable_structures::StableCell;
use serde::Deserialize;
use std::cell::RefCell;
use types::EventType;

/// The two service principals the canister answers to: the quoter opens quotes, the
/// watcher reads them. Both are unset until a controller calls `set_roles`, and every
/// check in `guards` refuses while they are.
#[derive(CandidType, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Roles {
    pub quoter: Option<Principal>,
    pub watcher: Option<Principal>,
}

fn encode(roles: &Roles) -> Vec<u8> {
    candid::encode_one(roles).expect("roles encode")
}

thread_local! {
    // Same two-copy shape as the config: STORED is the record, ROLES is the cheap read.
    static ROLES: RefCell<Roles> = RefCell::new(Roles::default());

    static STORED: RefCell<StableCell<Vec<u8>, Memory>> = RefCell::new(
        StableCell::init(roles_memory(), encode(&Roles::default()))
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

/// Writes both copies and records the change in the log. The caller does the
/// authorization; this is the storage path.
pub fn set_roles(quoter: Principal, watcher: Principal) -> Result<(), String> {
    // before anything is written: a rejected pair must leave no event and no cell write.
    // The anonymous principal is every unauthenticated caller at once, so a role held by
    // it is a role held by the world.
    for (name, p) in [("quoter", quoter), ("watcher", watcher)] {
        if p == Principal::anonymous() {
            return Err(format!("{name} cannot be the anonymous principal"));
        }
    }
    let roles = Roles {
        quoter: Some(quoter),
        watcher: Some(watcher),
    };
    // the log first, because it is the step that can refuse: a rotation is the one thing
    // that changes who may move money, so it is audited, and the cell below stays the
    // operative copy. Principals as text, which is what an operator reads in an alert.
    events::append_event(EventType::RolesChanged {
        quoter: quoter.to_text(),
        watcher: watcher.to_text(),
    })?;
    // out of stable memory is not a caller error, so it traps instead of returning Err:
    // a trap rolls the append above back with it, and an `Ok(Err(_))` would not
    STORED.with(|s| {
        s.borrow_mut()
            .set(encode(&roles))
            .expect("roles cell write")
    });
    ROLES.with(|r| *r.borrow_mut() = roles);
    Ok(())
}
