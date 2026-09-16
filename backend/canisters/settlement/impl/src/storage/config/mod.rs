use crate::storage::events;
use crate::storage::memory::{config_memory, Memory};
use ic_stable_structures::StableCell;
use std::cell::RefCell;
use types::config::{Config, IntervalChanges};
use types::EventType;

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

/// Writes the cell and records the change in the log. Callers do the authorization; this
/// is the storage path. Returns which timer intervals changed, so the caller restarts only
/// those timers.
pub fn set(new: Config) -> Result<IntervalChanges, String> {
    // before anything is written: a rejected config must leave no event and no cell write
    new.validate().map_err(|e| e.to_string())?;
    let changed = get().interval_changes(&new);
    // the log first, because it is the step that can refuse. The log is world-readable,
    // and the config's Debug prints every rpc url as `***`.
    events::append_event(EventType::ConfigChanged {
        json: format!("{new:?}"),
    })
    .map_err(|e| e.to_string())?;
    // out of stable memory is not a caller error, so it traps instead of returning Err:
    // a trap rolls the append above back with it, and an `Ok(Err(_))` would not
    CONFIG.with(|c| c.borrow_mut().set(new).expect("config cell write"));
    Ok(changed)
}
