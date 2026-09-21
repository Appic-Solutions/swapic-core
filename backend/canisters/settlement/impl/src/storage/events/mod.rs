use crate::state::transitions::{apply_state_transition, replay};
use crate::state::{State, Store};
use crate::storage::memory::{
    events_data_memory, events_index_memory, ledger_meta_memory, pockets_memory, swaps_memory,
    waiting_memory, Memory,
};
use ic_stable_structures::log::WriteError;
use ic_stable_structures::{StableBTreeMap, StableBTreeSet, StableCell, StableLog};
use std::cell::RefCell;
use thiserror::Error;
use types::canonical::CanonicalError;
use types::events::{chain_is_valid, Event, EventType};
use types::{
    ChainId, EventHash, EventIndex, LedgerMeta, Pocket, QuoteHash, Swap, Timestamp,
    TransitionError, WaitingKey,
};

/// Longest page a query will return, so one call can never walk the whole log.
const MAX_PAGE: u64 = 500;

thread_local! {
    // The log is the record and the four below are its fold. All five are private, so
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

    // The swaps waiting for their user, so the expiry sweep reads those and not every swap.
    static WAITING: RefCell<StableBTreeSet<WaitingKey, Memory>> =
        RefCell::new(StableBTreeSet::init(waiting_memory()));
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

    fn put_waiting(&mut self, key: WaitingKey) {
        WAITING.with(|waiting| waiting.borrow_mut().insert(key));
    }

    fn remove_waiting(&mut self, key: &WaitingKey) {
        WAITING.with(|waiting| waiting.borrow_mut().remove(key));
    }

    fn waiting(&self) -> Vec<WaitingKey> {
        WAITING.with(|waiting| waiting.borrow().iter().collect())
    }

    fn waiting_since_before(&self, cutoff: Timestamp, limit: usize) -> Vec<QuoteHash> {
        WAITING.with(|waiting| {
            waiting
                .borrow()
                .iter()
                .take_while(|key| key.since < cutoff)
                .take(limit)
                .map(|key| key.quote_hash)
                .collect()
        })
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
    #[error(transparent)]
    Canonical(#[from] CanonicalError),
}

/// Why the stable fold is out of step with the log it is the fold of.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum FoldOutOfStep {
    #[error(
        "the log holds {log_len} events but the fold expects to seal event {next_event_index}"
    )]
    Length {
        log_len: u64,
        next_event_index: EventIndex,
    },
    #[error("the log ends with {log} but the fold links to {fold}")]
    Head { log: EventHash, fold: EventHash },
}

impl From<FoldOutOfStep> for AppendError {
    fn from(error: FoldOutOfStep) -> Self {
        match error {
            FoldOutOfStep::Length {
                log_len,
                next_event_index,
            } => Self::IndexMismatch {
                log_len,
                sealed: next_event_index,
            },
            FoldOutOfStep::Head { log, fold } => Self::ChainDiverged { log, state: fold },
        }
    }
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
            AppendError::Canonical(error) => Self::Canonical(error.into()),
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
    WAITING.with(|_| ());
}

/// The chain head the log actually ends with: the last event's hash, or the zero hash at
/// genesis, which is the anchor `chain_is_valid` checks index 0 against. Decodes the last
/// event, and only that one.
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

/// The one and only path that writes an event, sealed at the canister's clock.
pub fn append_event(payload: EventType) -> Result<EventIndex, AppendError> {
    append_event_at(payload, Timestamp::from_nanos(ic_cdk::api::time()))
}

/// In O(1), whether the fold is in step with its log: the index it seals next is the log's
/// length, and the head it links to is the log's last hash, the zero hash on an empty log.
/// `post_upgrade` refuses an upgrade on it, and `append_event` refuses to write on it.
pub fn ensure_fold_in_step() -> Result<(), FoldOutOfStep> {
    let meta = StableStore(()).meta();
    let log_len = EVENTS.with(|e| e.borrow().len());
    if meta.next_event_index.get() != log_len {
        return Err(FoldOutOfStep::Length {
            log_len,
            next_event_index: meta.next_event_index,
        });
    }
    let head = log_head();
    if head != meta.last_event_hash {
        return Err(FoldOutOfStep::Head {
            log: head,
            fold: meta.last_event_hash,
        });
    }
    Ok(())
}

/// [`append_event`] sealed at `timestamp`: a caller that already read the clock seals on
/// the same reading, and a unit test runs without a canister. Guard, seal, append, apply,
/// all within a single message, so a trap anywhere rolls back the log and the fold together.
pub fn append_event_at(
    payload: EventType,
    timestamp: Timestamp,
) -> Result<EventIndex, AppendError> {
    // checked before anything is sealed: a head the log does not end with would seal on a
    // parent the log does not have and fork the chain at an index that still looks right,
    // and an index off the log's length would write one slot and apply another
    ensure_fold_in_step()?;
    let mut state = State::new(StableStore(()));
    state.check(&payload)?;
    let meta = state.meta();
    let index = meta.next_event_index;
    if index.next().is_none() {
        return Err(AppendError::LogFull);
    }
    // the guard refuses every amount without a preimage, so this is only the residue, and
    // it is refused before the write like everything above
    let event = Event::seal(index, timestamp, meta.last_event_hash, payload)?;
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
/// exactly. A log the heap fold refuses is a divergence too, so it answers false rather
/// than trapping before the audit can halt.
pub fn verify_replay() -> bool {
    match EVENTS.with(|e| replay(e.borrow().iter())) {
        Ok(rebuilt) => read_state(|live| rebuilt.matches(live)),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests;
