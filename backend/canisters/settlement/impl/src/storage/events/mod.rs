use crate::state::transitions::{apply_state_transition, replay};
use crate::state::{MemoryStore, State};
use crate::storage::memory::{events_data_memory, events_index_memory, Memory};
use ic_stable_structures::log::WriteError;
use ic_stable_structures::StableLog;
use std::cell::RefCell;
use thiserror::Error;
use types::events::{chain_is_valid, Event, EventType};
use types::{EventHash, EventIndex, Timestamp, TransitionError};

/// Longest page a query will return, so one call can never walk the whole log.
const MAX_PAGE: u64 = 500;

thread_local! {
    // The log is the record; both statics are private so `append_event` is the only writer.
    static EVENTS: RefCell<StableLog<Event, Memory, Memory>> = RefCell::new(
        StableLog::init(events_index_memory(), events_data_memory()).expect("event log init"),
    );

    // Derived from EVENTS and from nothing else: it is rebuilt from the log on upgrade.
    static STATE: RefCell<State<MemoryStore>> = RefCell::new(State::default());
}

/// Why an event was not appended. Nothing was written.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum AppendError {
    #[error("log head is {log} but the state carries {state}: the chain diverged")]
    ChainDiverged { log: EventHash, state: EventHash },
    #[error("log is at {log_len} but the event sealed as {sealed}")]
    IndexMismatch { log_len: u64, sealed: EventIndex },
    #[error("the log holds u64::MAX events")]
    LogFull,
    #[error("stable memory could not grow from {current_pages} by {delta_pages} pages")]
    OutOfStableMemory {
        current_pages: u64,
        delta_pages: u64,
    },
    #[error(transparent)]
    Transition(#[from] TransitionError),
}

/// The chain head the log actually ends with: the last event's hash, or the zero hash at
/// genesis, which is the anchor `chain_is_valid` checks index 0 against.
fn log_head() -> EventHash {
    EVENTS.with(|e| {
        let log = e.borrow();
        match log.len().checked_sub(1) {
            None => EventHash::ZERO,
            Some(last) => {
                log.get(last)
                    .expect("BUG: StableLog::get answers every index below len")
                    .hash
            }
        }
    })
}

/// The one and only path that writes an event: guard, seal, stable append, apply, all
/// within a single message. Nothing else may touch EVENTS or STATE.
pub fn append_event(payload: EventType) -> Result<EventIndex, AppendError> {
    STATE.with(|cell| {
        let mut state = cell.borrow_mut();
        let meta = state.meta();
        // the head the state links to must be the head the log ends with, checked before
        // anything is sealed: a diverged head would seal on a parent the log does not have
        // and fork the chain at an index that still looks right
        let head = log_head();
        if head != meta.last_event_hash {
            return Err(AppendError::ChainDiverged {
                log: head,
                state: meta.last_event_hash,
            });
        }
        state.check(&payload)?;
        let index = meta.next_event_index;
        if index.next().is_none() {
            return Err(AppendError::LogFull);
        }
        // the sealed index must be the slot the log is about to write, checked before the
        // write, because returning Err after one would commit an event never applied
        let log_len = EVENTS.with(|e| e.borrow().len());
        if log_len != index.get() {
            return Err(AppendError::IndexMismatch {
                log_len,
                sealed: index,
            });
        }
        let event = Event::seal(
            index,
            Timestamp::from_nanos(ic_cdk::api::time()),
            meta.last_event_hash,
            payload,
        );
        EVENTS.with(|e| e.borrow_mut().append(&event)).map_err(
            |WriteError::GrowFailed {
                 current_size,
                 delta,
             }| {
                AppendError::OutOfStableMemory {
                    current_pages: current_size,
                    delta_pages: delta,
                }
            },
        )?;
        // the append is the last fallible step and applying refuses nothing, so once the
        // message commits the log and the state agree; a trap rolls back both
        apply_state_transition(&mut state, &event);
        Ok(index)
    })
}

/// Read-only view of the live state. There is deliberately no mutable twin.
pub fn read_state<R>(f: impl FnOnce(&State<MemoryStore>) -> R) -> R {
    STATE.with(|s| f(&s.borrow()))
}

/// Test-only: moves the state's idea of the chain head off the log's, and touches the log
/// not at all. That is the one divergence the append-time head check exists for, and no
/// legitimate call can produce it. Flipping the same bit a second time puts the head back.
#[cfg(feature = "inttest")]
pub fn test_skew_chain_head() {
    STATE.with(|s| s.borrow_mut().skew_chain_head());
}

/// Called from `init` and `post_upgrade`: the heap is a cache, the log is the record.
pub fn rebuild_state_from_log() {
    let rebuilt = EVENTS.with(|e| replay(e.borrow().iter()));
    STATE.with(|s| *s.borrow_mut() = rebuilt);
}

pub fn event_count() -> u64 {
    EVENTS.with(|e| e.borrow().len())
}

pub fn events_page(start: u64, len: u64) -> Vec<Event> {
    // a page reaching past the last index simply ends there
    let end = start.saturating_add(len.min(MAX_PAGE));
    EVENTS.with(|e| {
        let log = e.borrow();
        (start..end).map_while(|i| log.get(i)).collect()
    })
}

/// Genesis-anchored: every link from index 0 up. Streams the log rather than
/// materializing it.
pub fn verify_chain() -> bool {
    EVENTS.with(|e| chain_is_valid(e.borrow().iter()))
}

/// The audit that matters: folding the log must reproduce the live state exactly.
pub fn verify_replay() -> bool {
    let rebuilt = EVENTS.with(|e| replay(e.borrow().iter()));
    read_state(|live| *live == rebuilt)
}
