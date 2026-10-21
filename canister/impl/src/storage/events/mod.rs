use crate::state::transitions::{apply_state_transition, replay, replay_into, ReplayError};
use crate::state::{after_bound, MemoryStore, State, Store};
use crate::storage::memory::{
    auto_refund_waiting_memory, books_memory, events_data_memory, events_index_memory,
    flows_memory, ledger_meta_memory, nonces_memory, pockets_memory, pool_credited_memory,
    pool_deposits_memory, pool_ledger_memory, pool_orphans_memory, pool_payout_intents_memory,
    pool_payout_queue_memory, pool_payouts_memory, pool_shares_memory, pool_withdrawal_nonces_memory, signed_memory,
    swaps_memory, unsigned_nonces_memory, Memory,
};
use ic_stable_structures::log::WriteError;
use ic_stable_structures::{StableBTreeMap, StableBTreeSet, StableCell, StableLog};
use std::cell::RefCell;
use std::ops::{Bound, RangeFrom};
use thiserror::Error;
use types::books::Books;
use types::canonical::CanonicalError;
use types::events::{chain_is_valid, check_link, Event, EventType, LinkError};
use types::hash::{OwnerKey, PoolRef};
use types::pool::{CreditKey, EvmPayout, NonceUse, PoolLedger, Registered, ShareLine};
use types::{
    ChainId, EventHash, EventIndex, Flow, LedgerMeta, Nonce, NonceKey, Pocket, QuoteHash, Swap,
    Timestamp, TransitionError, UnsignedTx, WaitingKey,
};

/// Longest page a query will return, so one call can never walk the whole log.
const MAX_PAGE: u64 = 500;

thread_local! {
    // The log is the record and the six below are its fold. All seven are private, and
    // every write to them goes through `append_event`: even the repair of a waiting entry
    // no event could have produced is an event, so the fold has exactly one writer.
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

    // The waiting swaps the expiry timer can act on: waiting for their user, and asking for
    // an automatic refund. So a pass reads those and not every swap, nor every waiting swap.
    static AUTO_REFUND_WAITING: RefCell<StableBTreeSet<WaitingKey, Memory>> =
        RefCell::new(StableBTreeSet::init(auto_refund_waiting_memory()));

    // The nonce allocator, one counter per chain. Part of the fold: every counter is the
    // number of `TxCreated` lines the log holds for its chain, so a replay reproduces it
    // and the deep audit compares it.
    static NONCES: RefCell<StableBTreeMap<ChainId, Nonce, Memory>> =
        RefCell::new(StableBTreeMap::init(nonces_memory()));

    // The nonces handed out and not yet signed for. Part of the fold as well: `TxCreated`
    // writes one, `TxSigned` and `TxCancelled` take it away, so a replay reproduces it and
    // the deep audit compares it. It is what rule A5 is read off, and the pass that ends an
    // abandoned allocation is the only reader.
    static UNSIGNED_NONCES: RefCell<StableBTreeMap<NonceKey, UnsignedTx, Memory>> =
        RefCell::new(StableBTreeMap::init(unsigned_nonces_memory()));

    // The canister's flows: relays, ICP to EVM and EVM to ICP. Part of the fold: only
    // `Flow` lines write them.
    static FLOWS: RefCell<StableBTreeMap<QuoteHash, Flow, Memory>> =
        RefCell::new(StableBTreeMap::init(flows_memory()));

    // The lines that record a signed transaction's bytes, by log index: what `signed_txs`
    // pages through. Part of the fold: only those lines write it.
    static SIGNED: RefCell<StableBTreeSet<EventIndex, Memory>> =
        RefCell::new(StableBTreeSet::init(signed_memory()));

    // Every chain's USDC books. Part of the fold: only `Books` lines write them.
    static BOOKS: RefCell<StableBTreeMap<ChainId, Books, Memory>> =
        RefCell::new(StableBTreeMap::init(books_memory()));

    // The pool: its ledger, its deposits by ref, its shares by account key, and every
    // transfer it credited. Part of the fold: only `Pool` lines write them.
    static POOL_LEDGER: RefCell<StableCell<PoolLedger, Memory>> = RefCell::new(
        StableCell::init(pool_ledger_memory(), PoolLedger::default())
            .expect("pool ledger cell init"),
    );

    static POOL_DEPOSITS: RefCell<StableBTreeMap<PoolRef, Registered, Memory>> =
        RefCell::new(StableBTreeMap::init(pool_deposits_memory()));

    static POOL_SHARES: RefCell<StableBTreeMap<OwnerKey, ShareLine, Memory>> =
        RefCell::new(StableBTreeMap::init(pool_shares_memory()));

    static POOL_CREDITED: RefCell<StableBTreeSet<CreditKey, Memory>> =
        RefCell::new(StableBTreeSet::init(pool_credited_memory()));

    // The Base payouts by id, the Base queue by place, and every withdrawal nonce a staker
    // used. Part of the fold: only `Pool` lines write them.
    static POOL_PAYOUTS: RefCell<StableBTreeMap<u64, EvmPayout, Memory>> =
        RefCell::new(StableBTreeMap::init(pool_payouts_memory()));

    static POOL_PAYOUT_QUEUE: RefCell<StableBTreeMap<u64, u64, Memory>> =
        RefCell::new(StableBTreeMap::init(pool_payout_queue_memory()));

    static POOL_WITHDRAWAL_NONCES: RefCell<StableBTreeSet<NonceUse, Memory>> =
        RefCell::new(StableBTreeSet::init(pool_withdrawal_nonces_memory()));

    // The paid payouts' Eco intents: what an Eco refund to the canister is matched by.
    static POOL_PAYOUT_INTENTS: RefCell<StableBTreeMap<PoolRef, u64, Memory>> =
        RefCell::new(StableBTreeMap::init(pool_payout_intents_memory()));

    // The orphans by credit key: held for a controller to refund or book.
    static POOL_ORPHANS: RefCell<StableBTreeMap<CreditKey, types::pool::Orphan, Memory>> =
        RefCell::new(StableBTreeMap::init(pool_orphans_memory()));
}

/// The fold in stable memory. Only this module can build one, so `append_event` is its one
/// and only writer.
pub struct StableStore(());

impl Store for StableStore {
    fn flow(&self, quote_hash: &QuoteHash) -> Option<Flow> {
        FLOWS.with(|flows| flows.borrow().get(quote_hash))
    }

    fn put_flow(&mut self, quote_hash: QuoteHash, flow: Flow) {
        FLOWS.with(|flows| flows.borrow_mut().insert(quote_hash, flow));
    }

    fn flows(&self) -> Vec<(QuoteHash, Flow)> {
        FLOWS.with(|flows| flows.borrow().iter().collect())
    }

    fn swap(&self, quote_hash: &QuoteHash) -> Option<Swap> {
        SWAPS.with(|swaps| swaps.borrow().get(quote_hash))
    }

    fn put_swap(&mut self, quote_hash: QuoteHash, swap: Swap) {
        SWAPS.with(|swaps| swaps.borrow_mut().insert(quote_hash, swap));
    }

    fn swaps(&self) -> Vec<(QuoteHash, Swap)> {
        SWAPS.with(|swaps| swaps.borrow().iter().collect())
    }

    fn swaps_after(&self, after: Option<QuoteHash>, limit: usize) -> Vec<(QuoteHash, Swap)> {
        SWAPS.with(|swaps| {
            swaps
                .borrow()
                .range((after_bound(after), Bound::Unbounded))
                .take(limit)
                .collect()
        })
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

    fn put_auto_refund_waiting(&mut self, key: WaitingKey) {
        AUTO_REFUND_WAITING.with(|waiting| waiting.borrow_mut().insert(key));
    }

    fn remove_auto_refund_waiting(&mut self, key: &WaitingKey) {
        AUTO_REFUND_WAITING.with(|waiting| waiting.borrow_mut().remove(key));
    }

    fn next_nonce(&self, chain_id: &ChainId) -> Nonce {
        NONCES
            .with(|nonces| nonces.borrow().get(chain_id))
            .unwrap_or(Nonce::ZERO)
    }

    fn put_next_nonce(&mut self, chain_id: ChainId, nonce: Nonce) {
        NONCES.with(|nonces| nonces.borrow_mut().insert(chain_id, nonce));
    }

    fn nonces(&self) -> Vec<(ChainId, Nonce)> {
        NONCES.with(|nonces| nonces.borrow().iter().collect())
    }

    fn put_signed(&mut self, index: EventIndex) {
        SIGNED.with(|signed| signed.borrow_mut().insert(index));
    }

    fn book(&self, chain_id: &ChainId) -> Option<Books> {
        BOOKS.with(|books| books.borrow().get(chain_id))
    }

    fn put_book(&mut self, chain_id: ChainId, entry: Books) {
        BOOKS.with(|books| books.borrow_mut().insert(chain_id, entry));
    }

    fn books(&self) -> Vec<(ChainId, Books)> {
        BOOKS.with(|books| books.borrow().iter().collect())
    }

    fn signed_in(&self, range: RangeFrom<EventIndex>, limit: usize) -> Vec<EventIndex> {
        SIGNED.with(|signed| signed.borrow().range(range).take(limit).collect())
    }

    fn pool_ledger(&self) -> PoolLedger {
        POOL_LEDGER.with(|ledger| ledger.borrow().get().clone())
    }

    fn put_pool_ledger(&mut self, ledger: PoolLedger) {
        // out of stable memory is not a caller error, so it traps, rolling back the message
        POOL_LEDGER.with(|cell| {
            cell.borrow_mut()
                .set(ledger)
                .expect("pool ledger cell write")
        });
    }

    fn pool_deposit(&self, r#ref: &PoolRef) -> Option<Registered> {
        POOL_DEPOSITS.with(|deposits| deposits.borrow().get(r#ref))
    }

    fn put_pool_deposit(&mut self, r#ref: PoolRef, deposit: Registered) {
        POOL_DEPOSITS.with(|deposits| deposits.borrow_mut().insert(r#ref, deposit));
    }

    fn pool_deposits(&self) -> Vec<(PoolRef, Registered)> {
        POOL_DEPOSITS.with(|deposits| deposits.borrow().iter().collect())
    }

    fn pool_share(&self, owner: &OwnerKey) -> Option<ShareLine> {
        POOL_SHARES.with(|shares| shares.borrow().get(owner))
    }

    fn put_pool_share(&mut self, owner: OwnerKey, line: ShareLine) {
        POOL_SHARES.with(|shares| shares.borrow_mut().insert(owner, line));
    }

    fn pool_shares(&self) -> Vec<(OwnerKey, ShareLine)> {
        POOL_SHARES.with(|shares| shares.borrow().iter().collect())
    }

    fn is_pool_credited(&self, key: &CreditKey) -> bool {
        POOL_CREDITED.with(|credited| credited.borrow().contains(key))
    }

    fn put_pool_credited(&mut self, key: CreditKey) {
        POOL_CREDITED.with(|credited| credited.borrow_mut().insert(key));
    }

    fn pool_credited(&self) -> Vec<CreditKey> {
        POOL_CREDITED.with(|credited| credited.borrow().iter().collect())
    }

    fn pool_payout(&self, id: u64) -> Option<EvmPayout> {
        POOL_PAYOUTS.with(|payouts| payouts.borrow().get(&id))
    }

    fn put_pool_payout(&mut self, id: u64, payout: EvmPayout) {
        POOL_PAYOUTS.with(|payouts| payouts.borrow_mut().insert(id, payout));
    }

    fn pool_payouts(&self) -> Vec<(u64, EvmPayout)> {
        POOL_PAYOUTS.with(|payouts| payouts.borrow().iter().collect())
    }

    fn put_payout_queued(&mut self, seq: u64, id: u64) {
        POOL_PAYOUT_QUEUE.with(|queue| queue.borrow_mut().insert(seq, id));
    }

    fn remove_payout_queued(&mut self, seq: u64) {
        POOL_PAYOUT_QUEUE.with(|queue| queue.borrow_mut().remove(&seq));
    }

    fn payout_queue(&self, limit: usize) -> Vec<(u64, u64)> {
        POOL_PAYOUT_QUEUE.with(|queue| queue.borrow().iter().take(limit).collect())
    }

    fn is_withdrawal_nonce_used(&self, key: &NonceUse) -> bool {
        POOL_WITHDRAWAL_NONCES.with(|nonces| nonces.borrow().contains(key))
    }

    fn put_withdrawal_nonce(&mut self, key: NonceUse) {
        POOL_WITHDRAWAL_NONCES.with(|nonces| nonces.borrow_mut().insert(key));
    }

    fn withdrawal_nonces(&self) -> Vec<NonceUse> {
        POOL_WITHDRAWAL_NONCES.with(|nonces| nonces.borrow().iter().collect())
    }

    fn payout_by_intent(&self, intent: &PoolRef) -> Option<u64> {
        POOL_PAYOUT_INTENTS.with(|intents| intents.borrow().get(intent))
    }

    fn put_payout_intent(&mut self, intent: PoolRef, id: u64) {
        POOL_PAYOUT_INTENTS.with(|intents| intents.borrow_mut().insert(intent, id));
    }

    fn payout_intents(&self) -> Vec<(PoolRef, u64)> {
        POOL_PAYOUT_INTENTS.with(|intents| intents.borrow().iter().collect())
    }

    fn pool_orphan(&self, key: &CreditKey) -> Option<types::pool::Orphan> {
        POOL_ORPHANS.with(|orphans| orphans.borrow().get(key))
    }

    fn put_pool_orphan(&mut self, key: CreditKey, orphan: types::pool::Orphan) {
        POOL_ORPHANS.with(|orphans| orphans.borrow_mut().insert(key, orphan));
    }

    fn pool_orphans(&self) -> Vec<(CreditKey, types::pool::Orphan)> {
        POOL_ORPHANS.with(|orphans| orphans.borrow().iter().collect())
    }

    fn unsigned_nonce(&self, key: &NonceKey) -> Option<UnsignedTx> {
        UNSIGNED_NONCES.with(|unsigned| unsigned.borrow().get(key))
    }

    fn put_unsigned_nonce(&mut self, key: NonceKey, tx: UnsignedTx) {
        UNSIGNED_NONCES.with(|unsigned| unsigned.borrow_mut().insert(key, tx));
    }

    fn remove_unsigned_nonce(&mut self, key: &NonceKey) {
        UNSIGNED_NONCES.with(|unsigned| unsigned.borrow_mut().remove(key));
    }

    fn unsigned_nonces(&self) -> Vec<(NonceKey, UnsignedTx)> {
        UNSIGNED_NONCES.with(|unsigned| unsigned.borrow().iter().collect())
    }

    fn waiting_keys(
        &self,
        before: Option<Timestamp>,
        naming: Option<QuoteHash>,
        limit: usize,
    ) -> Vec<WaitingKey> {
        AUTO_REFUND_WAITING.with(|waiting| {
            waiting
                .borrow()
                .iter()
                .take_while(|key| before.is_none_or(|cutoff| key.since < cutoff))
                .filter(|key| naming.is_none_or(|quote_hash| key.quote_hash == quote_hash))
                .take(limit)
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
    AUTO_REFUND_WAITING.with(|_| ());
    NONCES.with(|_| ());
    UNSIGNED_NONCES.with(|_| ());
    FLOWS.with(|_| ());
    SIGNED.with(|_| ());
    BOOKS.with(|_| ());
    POOL_LEDGER.with(|_| ());
    POOL_DEPOSITS.with(|_| ());
    POOL_SHARES.with(|_| ());
    POOL_CREDITED.with(|_| ());
    POOL_PAYOUTS.with(|_| ());
    POOL_PAYOUT_QUEUE.with(|_| ());
    POOL_WITHDRAWAL_NONCES.with(|_| ());
    POOL_PAYOUT_INTENTS.with(|_| ());
    POOL_ORPHANS.with(|_| ());
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
    // the engine acts the moment a step completes: a line that gives a swap its next move
    // arms one coalesced drive, and the tick is only the backstop. A trap later in this
    // message rolls the timer back with the line.
    if event.payload.moves_a_step() {
        crate::engine::arm();
    }
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

/// The event at `index`, if the log holds one.
pub fn event_at(index: u64) -> Option<Event> {
    EVENTS.with(|e| e.borrow().get(index))
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
///
/// Its cost grows with the log, so no timer runs it: a query that runs out of instructions
/// fails that query alone, and the ops door onto the same comparison is the controller's
/// `audit_replay_step`, which folds the log through [`replay_next`] a bounded step at a
/// time.
pub fn verify_replay() -> bool {
    match EVENTS.with(|e| replay(e.borrow().iter())) {
        Ok(rebuilt) => read_state(|live| rebuilt.matches(live)),
        Err(_) => false,
    }
}

/// How far a chunk of the chain got, and what the next chunk resumes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChainChunk {
    /// where the next pass starts, and the hash it checks that entry's parent against
    pub next_index: EventIndex,
    pub parent_hash: EventHash,
    /// links verified by this pass
    pub verified: u64,
    /// whether the pass reached the end of the log
    pub reached_head: bool,
}

/// Verifies at most `max` links from `cursor`, carrying its parent hash in: each entry must
/// sit where it says, link to the entry before it, and hash to the hash it carries. Streams
/// the log one entry at a time, so a pass costs its chunk and never the whole chain. A pass
/// that starts past the head verifies nothing and reports it reached the head.
pub fn verify_chain_chunk(
    from: EventIndex,
    parent: EventHash,
    max: u64,
) -> Result<ChainChunk, LinkError> {
    let log_len = event_count();
    let start = from.get().min(log_len);
    let end = start.saturating_add(max).min(log_len);
    let mut parent_hash = parent;
    let mut index = EventIndex::new(start);
    EVENTS.with(|e| {
        let log = e.borrow();
        for at in start..end {
            let event = log
                .get(at)
                .expect("BUG: StableLog::get answers every index below len");
            parent_hash = check_link(&event, index, parent_hash)?;
            index = index
                .next()
                .expect("BUG: an index below the log's length has a successor");
        }
        Ok(ChainChunk {
            next_index: index,
            parent_hash,
            verified: end - start,
            reached_head: end == log_len,
        })
    })
}

/// Folds at most `max` more entries of the log onto `fold`, from the index it seals next,
/// admitting each the way `append_event` did, and answers how many entries still lie
/// between the fold and the head. Reads the log one entry at a time, so a step costs the
/// entries it folds and never the whole log; the fold is the caller's to keep between
/// steps. A fold sealing past the head folds nothing and has nothing left, and the
/// comparison it then goes to is what refuses it.
pub fn replay_next(fold: &mut State<MemoryStore>, max: u64) -> Result<u64, ReplayError> {
    let log_len = event_count();
    let start = fold.meta().next_event_index.get();
    let end = start.saturating_add(max).min(log_len);
    EVENTS.with(|e| {
        let log = e.borrow();
        replay_into(fold, (start..end).map_while(|i| log.get(i)))
    })?;
    Ok(log_len.saturating_sub(fold.meta().next_event_index.get()))
}

impl From<ReplayError> for settlement_api::types::events::ReplayError {
    fn from(error: ReplayError) -> Self {
        match error {
            ReplayError::OutOfSequence { expected, found } => Self::OutOfSequence {
                expected: expected.get(),
                found: found.get(),
            },
            ReplayError::Unlinked {
                index,
                parent,
                head,
            } => Self::Unlinked {
                index: index.get(),
                parent: parent.into_bytes(),
                head: head.into_bytes(),
            },
            ReplayError::LogFull { index } => Self::LogFull { index: index.get() },
            ReplayError::Refused { index, error } => Self::Refused {
                index: index.get(),
                error: error.into(),
            },
        }
    }
}

/// Test-only: writes `event` into the log and moves the fold's index and head onto it, so
/// the fold stays in step with an entry no guard admitted. The only way to plant the broken
/// link the chain audit exists to find: `append_event` seals every entry on the head the log
/// actually ends with, and the O(1) head check would see a log the fold did not follow.
#[cfg(test)]
pub(crate) fn test_push_raw(event: Event) {
    let (index, hash) = (event.index, event.hash);
    EVENTS.with(|e| e.borrow_mut().append(&event).expect("raw log append"));
    let mut store = StableStore(());
    let meta = store.meta();
    store.put_meta(LedgerMeta {
        next_event_index: index.next().expect("a test index has a successor"),
        last_event_hash: hash,
        ..meta
    });
}

#[cfg(test)]
mod tests;
