//! The engine's backstop: a one-shot tick that comes back every `backstop_interval` while
//! the pool has work open (`pool::work_is_open`), for what no line announces: a deadline, a
//! balance landing on a ledger, a read worth trying again. Each tick drives the payouts
//! (`engine::drive_all`) and the moves (`rebalance::tick`). It lapses when nothing is open:
//! every pool line arms it, every drive arms it again while work is open, and the hourly pass
//! arms it as the safety, so an idle canister wakes for nothing.

#[cfg(test)]
mod tests;

use std::cell::Cell;
use types::Timestamp;

thread_local! {
    // Heap by necessity, like every timer: when the pending tick fires. An upgrade clears
    // it and `post_upgrade` arms again.
    static DUE: Cell<Option<Timestamp>> = const { Cell::new(None) };
}

/// Whether a tick due at `due` still lies ahead of `now`. The instant decides rather than a
/// flag, for the outbox's reason: a tick that cleared a flag and then trapped would leave it
/// set, and nothing would arm again.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
fn still_pending(due: Option<Timestamp>, now: Timestamp) -> bool {
    due.is_some_and(|due| due > now)
}

/// Puts a tick one interval out, unless one is pending. Off the canister there is no timer
/// to set.
pub fn arm() {
    #[cfg(target_arch = "wasm32")]
    {
        let now = Timestamp::from_nanos(ic_cdk::api::time());
        if still_pending(DUE.get(), now) {
            return;
        }
        let every = crate::storage::config::get().engine_every();
        ic_cdk_timers::set_timer(every, || ic_cdk::spawn(tick()));
        let due = now.as_nanos().saturating_add(every.as_nanos() as u64);
        DUE.set(Some(Timestamp::from_nanos(due)));
    }
}

/// Arms the tick while the pool has work open and the canister is not halted
/// (`set_halted` arms it when the halt lifts).
pub fn arm_if_open() {
    #[cfg(target_arch = "wasm32")]
    {
        if crate::storage::halt::is_halted() {
            return;
        }
        let now = Timestamp::from_nanos(ic_cdk::api::time());
        if crate::storage::events::read_state(|state| crate::pool::work_is_open(state, now)) {
            arm();
        }
    }
}

/// One tick: armed again first, before any await, so a trap past one cannot end the chain;
/// then the payouts and the moves.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
async fn tick() {
    arm_if_open();
    crate::engine::drive_all().await;
    super::rebalance::tick().await;
}
