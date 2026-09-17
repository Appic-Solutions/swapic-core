use crate::guards::require_controller;
use crate::storage::roles::{self, RolesError};
pub use candid::Principal;
use ic_cdk::update;
pub use settlement_api::types::errors::SetRolesError;

/// Controller-only, and it sets both roles at once: a deploy hands out the pair, and
/// there is no path that leaves one of them stale.
#[update]
pub fn set_roles(quoter: Principal, watcher: Principal) -> Result<(), SetRolesError> {
    require_controller().map_err(SetRolesError::Guard)?;
    roles::set_roles(quoter, watcher).map_err(|e| match e {
        RolesError::AnonymousRole(role) => SetRolesError::AnonymousRole(role),
        RolesError::Append(e) => SetRolesError::Append(e.into()),
    })
}
