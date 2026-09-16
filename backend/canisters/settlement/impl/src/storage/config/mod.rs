use crate::storage::events;
use crate::storage::memory::{config_memory, Memory};
use ic_stable_structures::StableCell;
use std::cell::RefCell;
use types::config::{Config, IntervalChanges};
use types::EventType;

thread_local! {
    // The live copy every reader sees. It is a cache of STORED, not a second source.
    static CONFIG: RefCell<Config> = RefCell::new(Config::default());

    // The copy that survives an upgrade: config is deploy-time truth, not something the
    // log replays, so it is held here rather than folded back out of the events.
    static STORED: RefCell<StableCell<Config, Memory>> = RefCell::new(
        StableCell::init(config_memory(), Config::default()).expect("config cell init"),
    );
}

/// Called from `init` and `post_upgrade`: on a fresh install this writes the defaults to
/// the cell, on an upgrade it reads back whatever was set. Update contexts only, because
/// the first touch of the cell grows stable memory.
pub fn load() {
    let stored = STORED.with(|s| s.borrow().get().clone());
    CONFIG.with(|c| *c.borrow_mut() = stored);
}

/// Cloned snapshot: there is no handle onto the live value.
pub fn get() -> Config {
    CONFIG.with(|c| c.borrow().clone())
}

/// Writes the cell and the heap copy, and records the change in the log. Callers do the
/// authorization; this is the storage path. Returns which timer intervals changed, so the
/// caller restarts only those timers.
pub fn set(new: Config) -> Result<IntervalChanges, String> {
    // before anything is written: a rejected config must leave no event and no cell write
    new.validate().map_err(|e| e.to_string())?;
    let changed = get().interval_changes(&new);
    // the log first, because it is the step that can refuse. The log is world-readable,
    // and the config's Debug prints every rpc url as `***`.
    events::append_event(EventType::ConfigChanged {
        json: format!("{new:?}"),
    })?;
    // out of stable memory is not a caller error, so it traps instead of returning Err:
    // a trap rolls the append above back with it, and an `Ok(Err(_))` would not
    STORED.with(|s| s.borrow_mut().set(new.clone()).expect("config cell write"));
    CONFIG.with(|c| *c.borrow_mut() = new);
    Ok(changed)
}
