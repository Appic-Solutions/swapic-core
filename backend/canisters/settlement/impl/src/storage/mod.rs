pub mod config;
pub mod events;
pub mod halt;
pub mod memory;
pub mod roles;

use crate::state::pending_quotes;

/// Touches every stable structure in an update context, so their headers are written here
/// and no query is ever the first to grow stable memory.
pub fn init() {
    events::init();
    config::init();
    roles::init();
    halt::init();
    pending_quotes::init();
}
