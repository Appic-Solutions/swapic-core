pub mod attestations;
pub mod audit_cursor;
pub mod chain_data;
pub mod config;
pub mod ecdsa_address;
pub mod events;
pub mod halt;
pub mod inflight;
pub mod memory;
pub mod outbox;
pub mod replay_cursor;
pub mod roles;
pub mod sanctions;

use crate::state::pending_quotes;

/// Touches every stable structure in an update context, so their headers are written here
/// and no query is ever the first to grow stable memory.
pub fn init() {
    events::init();
    config::init();
    roles::init();
    halt::init();
    pending_quotes::init();
    audit_cursor::init();
    chain_data::init();
    ecdsa_address::init();
    outbox::init();
    replay_cursor::init();
    sanctions::init();
    attestations::init();
    inflight::init();
}

/// Runs `f` on a thread of its own, so it starts on empty stable memory and leaves nothing
/// behind for the next test, however the harness schedules them. A failed assertion inside
/// fails the calling test with its own message.
#[cfg(test)]
pub(crate) fn on_fresh_memory<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static) -> R {
    std::thread::spawn(f)
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}
