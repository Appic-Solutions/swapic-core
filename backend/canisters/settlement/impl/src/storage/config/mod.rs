use crate::storage::ecdsa_address;
use crate::storage::events;
use crate::storage::events::AppendError;
use crate::storage::memory::{config_memory, Memory};
use ic_stable_structures::StableCell;
use std::cell::RefCell;
use thiserror::Error;
use types::config::{Config, ConfigError, IntervalChanges};
use types::EventType;

/// Why a config write was refused. Nothing was written.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SetConfigError {
    #[error(transparent)]
    Invalid(#[from] ConfigError),
    #[error(transparent)]
    Append(#[from] AppendError),
}

thread_local! {
    // Deploy-time truth rather than a fold of the log, so it has a cell of its own.
    static CONFIG: RefCell<StableCell<Config, Memory>> = RefCell::new(
        StableCell::init(config_memory(), Config::default()).expect("config cell init"),
    );
}

/// On a fresh install writes the defaults to the cell, in an update context, so no query
/// is ever the first to grow its memory.
pub fn init() {
    CONFIG.with(|_| ());
}

pub fn get() -> Config {
    CONFIG.with(|c| c.borrow().get().clone())
}

/// The line a config write leaves in the world-readable log: compact JSON of the redacted
/// wire view, so it reads in the field names and units operators set.
pub fn change_json(config: &Config) -> String {
    serde_json::to_string(&settlement_api::types::config::Config::from(config.clone())).expect(
        "BUG: the wire config is integers, strings, and maps keyed by integers, all of which JSON holds",
    )
}

/// Writes the cell and records the change in the log. Callers do the authorization; this
/// is the storage path. Returns which timer intervals changed, so the caller restarts only
/// those timers.
pub fn set(new: Config) -> Result<IntervalChanges, SetConfigError> {
    // before anything is written: a rejected config must leave no event and no cell write
    new.validate()?;
    ensure_key_name_is_not_moving(&new)?;
    let changed = get().interval_changes(&new);
    // the log first, because it is the step that can refuse. The log is world-readable,
    // and the redacted view prints every rpc url as `***`.
    events::append_event(EventType::ConfigChanged {
        json: change_json(&new),
    })?;
    // out of stable memory is not a caller error, so it traps instead of returning Err:
    // a trap rolls the append above back with it, and an `Ok(Err(_))` would not
    CONFIG.with(|c| c.borrow_mut().set(new).expect("config cell write"));
    Ok(changed)
}

/// The one rule a config write has that `Config::validate` cannot carry, because it is not
/// about the record: the key name is fixed once this canister has derived its address.
///
/// Signatures are made under the key the name chooses, and the address is derived from that
/// key, so a name change after the derivation would sign under one key while the cached
/// address, the world-readable `evm_address` query and every vault on every chain still
/// name the other. The parity trial recovers against the cached address, so every signature
/// would fail closed rather than come out wrong, but the canister would be unable to send
/// anything at all and each attempt would spend a nonce getting there.
///
/// The cell is deliberately not keyed by the name and invalidated on a change. Re-deriving
/// would move the address the vaults are configured to obey and the gas account that is
/// funded, which is a deploy ceremony and not a config write.
fn ensure_key_name_is_not_moving(new: &Config) -> Result<(), SetConfigError> {
    let current = get().ecdsa_key_name;
    if ecdsa_address::get().is_some() && new.ecdsa_key_name != current {
        return Err(SetConfigError::Invalid(ConfigError::EcdsaKeyNameFixed {
            current,
            requested: new.ecdsa_key_name.clone(),
        }));
    }
    Ok(())
}

/// Test-only: writes the cell and no log line, so a unit test can move a knob without a
/// canister clock to seal an event on. The line a write leaves is an audit record and no
/// part of the fold, so nothing the replay audit compares depends on it.
#[cfg(test)]
pub(crate) fn test_set(config: Config) {
    CONFIG.with(|c| c.borrow_mut().set(config).expect("config cell write"));
}

#[cfg(test)]
mod tests;
