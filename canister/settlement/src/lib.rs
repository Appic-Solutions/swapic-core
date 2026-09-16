use candid::Principal;
use ic_cdk::{init, post_upgrade, query, update};

pub mod auth;
pub mod config;
pub mod events;
pub mod log;
pub mod quote;
pub mod state;
pub mod timers;

#[init]
fn init() {
    // touch the log and every cell in an update context so the stable headers are
    // written here and no query is ever the first to grow stable memory
    log::rebuild_state_from_log();
    config::load();
    auth::load();
    // reads the intervals out of the config above, so it goes last
    timers::start_timers();
}

#[post_upgrade]
fn post_upgrade() {
    // the heap holds nothing the log cannot rebuild, so there is no pre_upgrade to match
    log::rebuild_state_from_log();
    // except the config and the roles, which are deploy-time truth rather than a fold of
    // the events. The pending quotes are neither: they are pre-money and come back empty.
    config::load();
    auth::load();
    // timers live in the heap, so an upgrade clears them and they are wired again here
    timers::start_timers();
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

/// Public, so it answers with the redacted view; `get_config_full` is the ops door onto
/// the real thing.
#[query]
fn get_config() -> config::Config {
    config::get().redacted()
}

/// The unredacted config, for ops. Controller-only, because `rpc_urls` holds api keys.
#[query]
fn get_config_full() -> Result<config::Config, String> {
    require_controller()?;
    Ok(config::get())
}

/// Controller-only, and it takes the whole record, so editing one knob is a
/// read-modify-write: read with `get_config_full`, never with the redacted `get_config`,
/// or the write puts "***" into `rpc_urls` and the canister loses its rpc access.
#[update]
fn set_config(new: config::Config) -> Result<(), String> {
    require_controller()?;
    config::set(new)
}

/// Controller-only, and it sets both roles at once: a deploy hands out the pair, and
/// there is no path that leaves one of them stale.
#[update]
fn set_roles(quoter: Principal, watcher: Principal) -> Result<(), String> {
    require_controller()?;
    auth::set_roles(quoter, watcher)
}

/// Quoter-only, and it takes a quote of either gas mode. Pre-money: it records a quote
/// the quoter has just handed a user so the funds that arrive later can be matched to it,
/// and returns the hash the user's deposit must carry. Nothing of value moves here, so
/// nothing is written to the log.
#[update]
fn register_quote(quote: quote::Quote) -> Result<events::Hash32, String> {
    auth::require_quoter()?;
    // deliberately no `gas_mode` gate: the store is a hash-to-quote lookup and the mode
    // only starts to matter when funds arrive. Do not add one.
    // seconds, to match the quote's own unit
    quote::register(quote, ic_cdk::api::time() / 1_000_000_000)
}

/// The folded state of one swap. Public: every field of it is already in the
/// world-readable event log, `quote_bytes` included.
#[query]
fn get_swap(quote_hash: events::Hash32) -> Option<state::SwapState> {
    log::with_state(|s| s.swaps.get(&quote_hash).cloned())
}

/// True once a replay audit has found the log and the live state disagree. Nothing clears
/// it but a controller, and money endpoints refuse while it is set.
#[query]
fn halted() -> bool {
    timers::is_halted()
}

/// Controller-only, both ways: `true` is an emergency stop, `false` is the human saying
/// the divergence has been investigated. The audit never clears it on its own.
#[update]
fn set_halted(halted: bool) -> Result<(), String> {
    require_controller()?;
    timers::set_halted(halted);
    Ok(())
}

/// Quoter or watcher. Not public: a pending quote carries the user's destination and
/// refund addresses.
#[query]
fn get_pending(quote_hash: events::Hash32) -> Result<Option<quote::Quote>, String> {
    auth::require_quoter_or_watcher()?;
    Ok(quote::get_pending(&quote_hash))
}

/// The ops rule: controllers only. The two service rules live in `auth`.
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

/// Test-only door onto the append chokepoint's head check: it flips one bit of the heap's
/// chain head and leaves the log untouched, which is the heap-versus-log divergence the
/// check and the replay audit exist for and which nothing else can produce. Behind the same
/// build feature and controller check as `test_append`, so neither reaches the production
/// interface. Calling it twice puts the head back.
#[cfg(feature = "test-endpoints")]
#[ic_cdk::update]
fn test_skew_state() -> Result<(), String> {
    require_controller()?;
    log::test_skew_chain_head();
    Ok(())
}

ic_cdk::export_candid!();
