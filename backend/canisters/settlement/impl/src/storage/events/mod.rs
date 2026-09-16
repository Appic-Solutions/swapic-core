use crate::state::transitions::{apply_state_transition, replay};
use crate::state::{LedgerMeta, State, Store};
use crate::storage::memory::{
    events_data_memory, events_index_memory, ledger_meta_memory, pockets_memory, swaps_memory,
    Memory,
};
use ic_stable_structures::log::WriteError;
use ic_stable_structures::{StableBTreeMap, StableCell, StableLog};
use std::cell::RefCell;
use thiserror::Error;
use types::events::{chain_is_valid, Event, EventType};
use types::{ChainId, EventHash, EventIndex, Pocket, QuoteHash, Swap, Timestamp, TransitionError};

/// Longest page a query will return, so one call can never walk the whole log.
const MAX_PAGE: u64 = 500;

thread_local! {
    // The log is the record and the three below are its fold. All four are private, so
    // `append_event` is the only writer of any of them.
    static EVENTS: RefCell<StableLog<Event, Memory, Memory>> = RefCell::new(
        StableLog::init(events_index_memory(), events_data_memory()).expect("event log init"),
    );

    static SWAPS: RefCell<StableBTreeMap<QuoteHash, Swap, Memory>> =
        RefCell::new(StableBTreeMap::init(swaps_memory()));

    static POCKETS: RefCell<StableBTreeMap<ChainId, Pocket, Memory>> =
        RefCell::new(StableBTreeMap::init(pockets_memory()));

    static LEDGER_META: RefCell<StableCell<LedgerMeta, Memory>> = RefCell::new(
        StableCell::init(ledger_meta_memory(), LedgerMeta::default())
            .expect("ledger meta cell init"),
    );
}

/// The fold in stable memory. Only this module can build one, so only `append_event`
/// ever holds it mutably.
pub struct StableStore(());

impl Store for StableStore {
    fn swap(&self, quote_hash: &QuoteHash) -> Option<Swap> {
        SWAPS.with(|swaps| swaps.borrow().get(quote_hash))
    }

    fn put_swap(&mut self, quote_hash: QuoteHash, swap: Swap) {
        SWAPS.with(|swaps| swaps.borrow_mut().insert(quote_hash, swap));
    }

    fn swaps(&self) -> Vec<(QuoteHash, Swap)> {
        SWAPS.with(|swaps| swaps.borrow().iter().collect())
    }

    fn pocket(&self, chain_id: &ChainId) -> Option<Pocket> {
        POCKETS.with(|pockets| pockets.borrow().get(chain_id))
    }

    fn put_pocket(&mut self, chain_id: ChainId, pocket: Pocket) {
        POCKETS.with(|pockets| pockets.borrow_mut().insert(chain_id, pocket));
    }

    fn pockets(&self) -> Vec<(ChainId, Pocket)> {
        POCKETS.with(|pockets| pockets.borrow().iter().collect())
    }

    fn meta(&self) -> LedgerMeta {
        LEDGER_META.with(|meta| *meta.borrow().get())
    }

    fn put_meta(&mut self, meta: LedgerMeta) {
        // out of stable memory is not a caller error, so it traps, rolling back the message
        LEDGER_META.with(|cell| cell.borrow_mut().set(meta).expect("ledger meta cell write"));
    }
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

impl From<AppendError> for settlement_api::types::errors::AppendError {
    fn from(error: AppendError) -> Self {
        match error {
            AppendError::ChainDiverged { log, state } => Self::ChainDiverged {
                log_head: log.into_bytes(),
                state_head: state.into_bytes(),
            },
            AppendError::IndexMismatch { log_len, sealed } => Self::IndexMismatch {
                log_len,
                sealed: sealed.get(),
            },
            AppendError::LogFull => Self::LogFull,
            AppendError::OutOfStableMemory {
                current_pages,
                delta_pages,
            } => Self::OutOfStableMemory {
                current_pages,
                delta_pages,
            },
            AppendError::Transition(error) => Self::Transition(error.into()),
        }
    }
}

/// Writes the headers of the log and of the fold, in an update context, so no query is
/// ever the first to grow their memory.
pub fn init() {
    EVENTS.with(|_| ());
    SWAPS.with(|_| ());
    POCKETS.with(|_| ());
    LEDGER_META.with(|_| ());
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

/// The one and only path that writes an event: guard, seal, append, apply, all within a
/// single message, so a trap anywhere rolls back the log and the fold together.
pub fn append_event(payload: EventType) -> Result<EventIndex, AppendError> {
    let mut state = State::new(StableStore(()));
    let meta = state.meta();
    // the head the fold links to must be the head the log ends with, checked before
    // anything is sealed: a diverged head would seal on a parent the log does not have and
    // fork the chain at an index that still looks right
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
         }| AppendError::OutOfStableMemory {
            current_pages: current_size,
            delta_pages: delta,
        },
    )?;
    // the append is the last step that returns an error and applying refuses nothing, so
    // once the message commits the log and the fold agree
    apply_state_transition(&mut state, &event);
    Ok(index)
}

/// Read-only view of the fold. There is deliberately no mutable twin.
pub fn read_state<R>(f: impl FnOnce(&State<StableStore>) -> R) -> R {
    f(&State::new(StableStore(())))
}

/// Test-only: moves the fold's idea of the chain head off the log's, and touches the log
/// not at all. That is the one divergence the append-time head check exists for, and no
/// legitimate call can produce it. Flipping the same bit a second time puts the head back.
#[cfg(feature = "inttest")]
pub fn test_skew_chain_head() {
    State::new(StableStore(())).skew_chain_head();
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

/// The audit that matters: folding the log onto the heap must reproduce the stable fold
/// exactly.
pub fn verify_replay() -> bool {
    let rebuilt = EVENTS.with(|e| replay(e.borrow().iter()));
    read_state(|live| live.matches(&rebuilt))
}

#[cfg(test)]
mod tests;
