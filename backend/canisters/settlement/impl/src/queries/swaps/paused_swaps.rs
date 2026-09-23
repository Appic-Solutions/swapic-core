use crate::engine;
use crate::state::Store;
use crate::storage::{config, events};
use ic_cdk::query;

/// World-readable. How many open swaps wait on a rail the deploy has turned off: each is
/// paused, not stopped, refused with a retryable `RailDisabled` on every engine tick, and
/// moves again once the rail is back on. A count and nothing else, so it says nothing about
/// any one swap that the public event log does not already. It reads every swap the fold
/// holds, which is what one engine tick reads too.
#[query]
pub fn paused_swaps() -> u64 {
    let config = config::get();
    events::read_state(|state| {
        let swaps = state.store().swaps();
        engine::paused_on_disabled_rails(&config, swaps.iter().map(|(_, swap)| swap))
    })
}
