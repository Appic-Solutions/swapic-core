use crate::events::{chain_is_valid, seal, Event, EventEnvelope, Hash32};
use crate::state::{apply, check_transition, replay, AppState};
use ic_stable_structures::memory_manager::{MemoryId, MemoryManager, VirtualMemory};
use ic_stable_structures::storable::Bound;
use ic_stable_structures::{DefaultMemoryImpl, StableLog, Storable};
use std::borrow::Cow;
use std::cell::RefCell;

pub(crate) type Memory = VirtualMemory<DefaultMemoryImpl>;

// assigned once, never reused for anything else
const INDEX_MEMORY: MemoryId = MemoryId::new(0);
const DATA_MEMORY: MemoryId = MemoryId::new(1);
pub(crate) const CONFIG_MEMORY: MemoryId = MemoryId::new(2);
pub(crate) const AUTH_MEMORY: MemoryId = MemoryId::new(3);
pub(crate) const HALT_MEMORY: MemoryId = MemoryId::new(4);

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
        StableLog::init(memory(INDEX_MEMORY), memory(DATA_MEMORY)).expect("event log init"),
    );

    // Derived from EVENTS and from nothing else: it is rebuilt from the log on upgrade.
    static STATE: RefCell<AppState> = RefCell::new(AppState::default());
}

/// The canister's one memory manager; every stable structure takes its pages from here,
/// under an id from the list above.
pub(crate) fn memory(id: MemoryId) -> Memory {
    MEMORY.with(|m| m.borrow().get(id))
}

/// The chain head the log actually ends with: the last envelope's hash, or the zero hash
/// at genesis, which is the anchor `chain_is_valid` checks index 0 against.
fn log_head() -> Result<Hash32, String> {
    EVENTS.with(|e| {
        let log = e.borrow();
        match log.len().checked_sub(1) {
            None => Ok([0u8; 32]),
            Some(last) => log.get(last).map(|env| env.hash).ok_or_else(|| {
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
fn short(hash: &Hash32) -> String {
    hex::encode(&hash[..4])
}

/// The one and only path that writes an event: guard, seal, stable append, apply, all
/// within a single message. Nothing else may touch EVENTS or STATE.
pub fn append_event(event: Event) -> Result<u64, String> {
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
        check_transition(&state, &event)?;
        let envelope = seal(
            state.next_event_index,
            ic_cdk::api::time(),
            state.last_event_hash,
            event,
        );
        // the sealed index must be the slot the log is about to write, or the chain forks;
        // checked before the write, because returning Err after one would commit an event
        // that was never applied
        let next_slot = EVENTS.with(|e| e.borrow().len());
        if next_slot != envelope.index {
            return Err(format!(
                "log is at {next_slot} but the event sealed as {}",
                envelope.index
            ));
        }
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

/// Test-only: moves the heap's idea of the chain head off the log's, and touches the log
/// not at all. That is the one divergence shape the append-time head check exists for, and
/// no legitimate call can produce it, so it cannot be reached from a test any other way.
/// Flipping the same bit a second time puts the head back.
#[cfg(feature = "test-endpoints")]
pub fn test_skew_chain_head() {
    STATE.with(|s| s.borrow_mut().last_event_hash[0] ^= 1);
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
