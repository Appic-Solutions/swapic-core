//! What the canister's doors and timers remember between calls (C-09, `types::timer_state`):
//! one stable cell, so an upgrade forgets none of it.

#[cfg(test)]
mod tests;

use crate::storage::memory::{timer_state_memory, Memory};
use ic_stable_structures::StableCell;
use std::cell::RefCell;
use types::timer_state::TimerState;

thread_local! {
    static STATE: RefCell<StableCell<TimerState, Memory>> = RefCell::new(
        StableCell::init(timer_state_memory(), TimerState::default())
            .expect("timer state cell init"),
    );
}

#[cfg(test)]
thread_local! {
    // how many times the cell was written, for the test that an unchanged state is not
    pub(crate) static WRITES: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// On a fresh install writes the empty state, in an update context.
pub fn init() {
    STATE.with(|_| ());
}

pub fn get() -> TimerState {
    STATE.with(|cell| cell.borrow().get().clone())
}

/// `f` on the stored state, which is written back only when `f` changed it.
pub fn update<R>(f: impl FnOnce(&mut TimerState) -> R) -> R {
    let before = get();
    let mut state = before.clone();
    let answer = f(&mut state);
    if state == before {
        return answer;
    }
    #[cfg(test)]
    WRITES.with(|writes| writes.set(writes.get() + 1));
    STATE.with(|cell| {
        cell.borrow_mut()
            .set(state)
            .expect("timer state cell write")
    });
    answer
}
