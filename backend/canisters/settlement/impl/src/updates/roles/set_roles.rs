use crate::guards::require_controller;
use crate::storage::roles;
pub use candid::Principal;
use ic_cdk::update;

/// Controller-only, and it sets both roles at once: a deploy hands out the pair, and
/// there is no path that leaves one of them stale.
#[update]
pub fn set_roles(quoter: Principal, watcher: Principal) -> Result<(), String> {
    require_controller()?;
    roles::set_roles(quoter, watcher)
}
