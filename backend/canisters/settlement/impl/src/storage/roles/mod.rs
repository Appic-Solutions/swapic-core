use crate::storage::events;
use crate::storage::memory::{roles_memory, Memory};
use candid::{CandidType, Principal};
use ic_stable_structures::storable::{Bound, Storable};
use ic_stable_structures::StableCell;
use serde::Deserialize;
use std::borrow::Cow;
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

/// Stored as candid. Roles that no longer decode trap, which leaves an upgrade on the wasm
/// that wrote them rather than quietly locking both services out.
impl Storable for Roles {
    fn to_bytes(&self) -> Cow<'_, [u8]> {
        Cow::Owned(candid::encode_one(self).expect("BUG: roles always encode as candid"))
    }

    fn from_bytes(bytes: Cow<[u8]>) -> Self {
        candid::decode_one(&bytes).expect("roles decode")
    }

    const BOUND: Bound = Bound::Unbounded;
}

thread_local! {
    // Deploy-time truth rather than a fold of the log, so it has a cell of its own.
    static ROLES: RefCell<StableCell<Roles, Memory>> = RefCell::new(
        StableCell::init(roles_memory(), Roles::default()).expect("roles cell init"),
    );
}

/// On a fresh install writes the unset roles to the cell, in an update context, so no
/// query is ever the first to grow its memory.
pub fn init() {
    ROLES.with(|_| ());
}

pub fn get() -> Roles {
    ROLES.with(|r| r.borrow().get().clone())
}

/// Writes the cell and records the change in the log. The caller does the authorization;
/// this is the storage path.
pub fn set_roles(quoter: Principal, watcher: Principal) -> Result<(), String> {
    // before anything is written: a rejected pair must leave no event and no cell write.
    // The anonymous principal is every unauthenticated caller at once, so a role held by
    // it is a role held by the world.
    for (name, p) in [("quoter", quoter), ("watcher", watcher)] {
        if p == Principal::anonymous() {
            return Err(format!("{name} cannot be the anonymous principal"));
        }
    }
    // the log first, because it is the step that can refuse: a rotation changes who may
    // move money, so it is audited. Principals as text, which is what an operator reads.
    events::append_event(EventType::RolesChanged {
        quoter: quoter.to_text(),
        watcher: watcher.to_text(),
    })
    .map_err(|e| e.to_string())?;
    let roles = Roles {
        quoter: Some(quoter),
        watcher: Some(watcher),
    };
    // out of stable memory is not a caller error, so it traps instead of returning Err:
    // a trap rolls the append above back with it, and an `Ok(Err(_))` would not
    ROLES.with(|r| r.borrow_mut().set(roles).expect("roles cell write"));
    Ok(())
}
