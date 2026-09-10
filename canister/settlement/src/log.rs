use crate::events::{chain_is_valid, seal, Event, EventEnvelope};
use crate::state::{apply, check_transition, replay, AppState};
use ic_stable_structures::memory_manager::{MemoryId, MemoryManager, VirtualMemory};
use ic_stable_structures::storable::Bound;
use ic_stable_structures::{DefaultMemoryImpl, StableLog, Storable};
use std::borrow::Cow;
use std::cell::RefCell;

type Memory = VirtualMemory<DefaultMemoryImpl>;

// assigned once, never reused for anything else
const INDEX_MEMORY: MemoryId = MemoryId::new(0);
const DATA_MEMORY: MemoryId = MemoryId::new(1);

/// Longest page a query will return, so one call can never walk the whole log.
const MAX_PAGE: u64 = 500;

// Storage codec only. The hash chain uses the hand-written codec in `events`, so a
// candid layout change can never move a hash.
impl Storable for EventEnvelope {
    fn to_bytes(&self) -> Cow<'_, [u8]> {
        Cow::Owned(candid::encode_one(self).expect("envelope encodes"))
    }

    fn from_bytes(bytes: Cow<[u8]>) -> Self {
        candid::decode_one(&bytes).expect("envelope decodes")
    }

    const BOUND: Bound = Bound::Unbounded;
}

thread_local! {
    static MEMORY: RefCell<MemoryManager<DefaultMemoryImpl>> =
        RefCell::new(MemoryManager::init(DefaultMemoryImpl::default()));

    // The log is the record; both statics are private so `append_event` is the only writer.
    static EVENTS: RefCell<StableLog<EventEnvelope, Memory, Memory>> = RefCell::new(
        StableLog::init(
            MEMORY.with(|m| m.borrow().get(INDEX_MEMORY)),
            MEMORY.with(|m| m.borrow().get(DATA_MEMORY)),
        )
        .expect("event log init"),
    );

    // Derived from EVENTS and from nothing else: it is rebuilt from the log on upgrade.
    static STATE: RefCell<AppState> = RefCell::new(AppState::default());
}

/// The one and only path that writes an event: guard, seal, stable append, apply, all
/// within a single message. Nothing else may touch EVENTS or STATE.
pub fn append_event(event: Event) -> Result<u64, String> {
    STATE.with(|cell| {
        let mut state = cell.borrow_mut();
        check_transition(&state, &event)?;
        let envelope = seal(
            state.next_event_index,
            ic_cdk::api::time(),
            state.last_event_hash,
            event,
        );
        EVENTS
            .with(|e| e.borrow_mut().append(&envelope))
            .map_err(|e| format!("stable append failed: {e:?}"))?;
        // the append is the last fallible step and `apply` is total, so once the message
        // commits the log and the state agree; if anything traps, the IC rolls back both
        apply(&mut state, &envelope);
        Ok(envelope.index)
    })
}

/// Read-only view of the live state. There is deliberately no mutable twin.
pub fn with_state<R>(f: impl FnOnce(&AppState) -> R) -> R {
    STATE.with(|s| f(&s.borrow()))
}

/// Called from `init` and `post_upgrade`: the heap is a cache, the log is the record.
pub fn rebuild_state_from_log() {
    let rebuilt = EVENTS.with(|e| replay(e.borrow().iter()));
    STATE.with(|s| *s.borrow_mut() = rebuilt);
}

pub fn event_count() -> u64 {
    EVENTS.with(|e| e.borrow().len())
}

pub fn events_page(start: u64, len: u64) -> Vec<EventEnvelope> {
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
