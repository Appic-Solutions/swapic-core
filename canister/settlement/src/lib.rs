use ic_cdk::{init, post_upgrade, query, update};

pub mod config;
pub mod events;
pub mod log;
pub mod state;

#[init]
fn init() {
    // touch the log and the config cell in an update context so the stable headers are
    // written here and no query is ever the first to grow stable memory
    log::rebuild_state_from_log();
    config::load();
}

#[post_upgrade]
fn post_upgrade() {
    // the heap holds nothing the log cannot rebuild, so there is no pre_upgrade to match
    log::rebuild_state_from_log();
    // except the config, which is deploy-time truth rather than a fold of the events
    config::load();
}

#[query]
fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

#[query]
fn event_count() -> u64 {
    log::event_count()
}

/// `len` is capped server-side; ask for the count first and page through.
#[query]
fn events_page(start: u64, len: u64) -> Vec<events::EventEnvelope> {
    log::events_page(start, len)
}

#[query]
fn verify_chain() -> bool {
    log::verify_chain()
}

#[query]
fn verify_replay() -> bool {
    log::verify_replay()
}

#[query]
fn get_config() -> config::Config {
    config::get()
}

#[update]
fn set_config(new: config::Config) -> Result<(), String> {
    require_controller()?;
    config::set(new)
}

/// The one authorization rule in the canister: controllers only.
fn require_controller() -> Result<(), String> {
    if ic_cdk::api::is_controller(&ic_cdk::api::caller()) {
        Ok(())
    } else {
        Err("not controller".to_string())
    }
}

/// Test-only door onto the log, behind a build feature and a controller check, and it
/// still goes through `append_event` like everything else.
#[cfg(feature = "test-endpoints")]
#[ic_cdk::update]
fn test_append(event: events::Event) -> Result<u64, String> {
    require_controller()?;
    log::append_event(event)
}

ic_cdk::export_candid!();
