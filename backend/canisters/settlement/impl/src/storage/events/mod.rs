use crate::state::transitions::{apply, check_transition, replay};
use crate::state::AppState;
use crate::storage::memory::{events_data_memory, events_index_memory, Memory};
use ic_stable_structures::StableLog;
use std::cell::RefCell;
use types::events::{chain_is_valid, Event, EventType};
use types::{EventHash, Timestamp};

/// Longest page a query will return, so one call can never walk the whole log.
const MAX_PAGE: u64 = 500;

thread_local! {
    // The log is the record; both statics are private so `append_event` is the only writer.
    static EVENTS: RefCell<StableLog<Event, Memory, Memory>> = RefCell::new(
        StableLog::init(events_index_memory(), events_data_memory()).expect("event log init"),
    );

    // Derived from EVENTS and from nothing else: it is rebuilt from the log on upgrade.
    static STATE: RefCell<AppState> = RefCell::new(AppState::default());
}

/// The chain head the log actually ends with: the last envelope's hash, or the zero hash
/// at genesis, which is the anchor `chain_is_valid` checks index 0 against.
fn log_head() -> Result<EventHash, String> {
    EVENTS.with(|e| {
        let log = e.borrow();
        match log.len().checked_sub(1) {
            None => Ok(EventHash::ZERO),
            Some(last) => log.get(last).map(|event| event.hash).ok_or_else(|| {
                format!(
                    "log says it holds {} entries but {last} is missing",
                    last + 1
                )
            }),
        }
    })
}

/// Enough of a hash to tell two chain heads apart in an error message, without printing
/// sixty-four characters of it.
fn short(hash: &EventHash) -> String {
    hex::encode(&hash.as_ref()[..4])
}

/// The one and only path that writes an event: guard, seal, stable append, apply, all
/// within a single message. Nothing else may touch EVENTS or STATE.
pub fn append_event(payload: EventType) -> Result<u64, String> {
    STATE.with(|cell| {
        let mut state = cell.borrow_mut();
        // the head the heap thinks the chain is on must be the head the log actually ends
        // with, checked before anything is sealed: `seal` takes its parent hash from the
        // state, so a diverged head would write an envelope linking to a parent the log does
        // not have and fork the chain permanently. The index check below cannot see this
        // shape, because the index would still be exactly right. Checked first, because a
        // state whose head is wrong is not one worth asking a transition question of.
        let head = log_head()?;
        if head != state.last_event_hash {
            return Err(format!(
                "log head is {} but the state carries {}: the chain diverged",
                short(&head),
                short(&state.last_event_hash)
            ));
        }
        check_transition(&state, &payload)?;
        let event = Event::seal(
            state.next_event_index,
            Timestamp::from_nanos(ic_cdk::api::time()),
            state.last_event_hash,
            payload,
        );
        // the sealed index must be the slot the log is about to write, or the chain forks;
        // checked before the write, because returning Err after one would commit an event
        // that was never applied
        let next_slot = EVENTS.with(|e| e.borrow().len());
        if next_slot != event.index.get() {
            return Err(format!(
                "log is at {next_slot} but the event sealed as {}",
                event.index
            ));
        }
        EVENTS
            .with(|e| e.borrow_mut().append(&event))
            .map_err(|e| format!("stable append failed: {e:?}"))?;
        // the append is the last fallible step and `apply` is total, so once the message
        // commits the log and the state agree; if anything traps, the IC rolls back both
        apply(&mut state, &event);
        Ok(event.index.get())
    })
}

/// Read-only view of the live state. There is deliberately no mutable twin.
pub fn with_state<R>(f: impl FnOnce(&AppState) -> R) -> R {
    STATE.with(|s| f(&s.borrow()))
}

/// Test-only: moves the heap's idea of the chain head off the log's, and touches the log
/// not at all. That is the one divergence shape the append-time head check exists for, and
/// no legitimate call can produce it, so it cannot be reached from a test any other way.
/// Flipping the same bit a second time puts the head back.
#[cfg(feature = "inttest")]
pub fn test_skew_chain_head() {
    STATE.with(|s| {
        let mut state = s.borrow_mut();
        let mut head = state.last_event_hash.into_bytes();
        head[0] ^= 1;
        state.last_event_hash = EventHash::new(head);
    });
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
    with_state(|live| *live == rebuilt)
}
