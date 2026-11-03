use crate::state::transitions::apply_state_transition;
use crate::state::{State, Store};
use crate::storage::memory::{
    books_memory, eco_rescues_memory, events_data_memory, events_index_memory,
    flow_by_source_memory, flow_by_tx_memory, flows_memory, ledger_meta_memory, nonces_memory,
    pool_credited_memory, pool_deposits_memory, pool_ledger_memory, pool_moves_memory,
    pool_orphans_memory, pool_own_sources_memory, pool_payout_intents_memory,
    pool_payout_queue_memory, pool_payouts_memory, pool_shares_memory,
    pool_withdrawal_nonces_memory, signed_memory, sweeps_memory, unsigned_nonces_memory, Memory,
};
use ic_stable_structures::log::WriteError;
use ic_stable_structures::{StableBTreeMap, StableBTreeSet, StableCell, StableLog};
use std::cell::RefCell;
use std::ops::RangeFrom;
use thiserror::Error;
use types::books::Books;
use types::events::{Event, EventType};
use types::hash::{OwnerKey, PoolRef};
use types::pool::{CreditKey, EvmPayout, NonceUse, PoolLedger, Registered, ShareLine};
use types::{
    ChainId, EventIndex, Flow, LedgerMeta, Nonce, NonceKey, QuoteHash, Timestamp, TransitionError,
    UnsignedTx,
};

/// Longest page a query will return, so one call can never walk the whole log.
const MAX_PAGE: u64 = 500;

thread_local! {
    // The log is the record and every map below is its fold. All are private, and every
    // write to them goes through `append_event` but one: `forget_seen`, which drops from the
    // books' unseen lists what a balance read already holds. The retired v1 swaps, pockets
    // and waits held memory ids 6, 7 and 10.
    static EVENTS: RefCell<StableLog<Event, Memory, Memory>> = RefCell::new(
        StableLog::init(events_index_memory(), events_data_memory()).expect("event log init"),
    );

    static LEDGER_META: RefCell<StableCell<LedgerMeta, Memory>> = RefCell::new(
        StableCell::init(ledger_meta_memory(), LedgerMeta::default())
            .expect("ledger meta cell init"),
    );

    // The nonce allocator, one counter per chain. Part of the fold: every counter is the
    // number of `TxCreated` lines the log holds for its chain, so a replay reproduces it
    // and the deep audit compares it.
    static NONCES: RefCell<StableBTreeMap<ChainId, Nonce, Memory>> =
        RefCell::new(StableBTreeMap::init(nonces_memory()));

    // The nonces handed out and not yet signed for. Part of the fold as well: `TxCreated`
    // writes one, the signed record and `TxCancelled` take it away, so a replay reproduces it and
    // the deep audit compares it. It is what rule A5 is read off, and the pass that ends an
    // abandoned allocation is the only reader.
    static UNSIGNED_NONCES: RefCell<StableBTreeMap<NonceKey, UnsignedTx, Memory>> =
        RefCell::new(StableBTreeMap::init(unsigned_nonces_memory()));

    // The canister's flows: the gasless relays and the CCTP finishes (and the retired ICP
    // flows' kinds). Part of the fold: only `Flow` lines write them.
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

    // The rebalance moves by id (P11c). Part of the fold: only `Pool` lines write them.
    static POOL_MOVES: RefCell<StableBTreeMap<u64, types::pool::Move, Memory>> =
        RefCell::new(StableBTreeMap::init(pool_moves_memory()));

    // The canister's own CCTP sources by transaction hash (P11c): a move's or a payout's
    // transaction, once it succeeded at depth, which its finish names with no fee step.
    static POOL_OWN_SOURCES: RefCell<StableBTreeMap<types::TxHash, types::pool::OwnSource, Memory>> =
        RefCell::new(StableBTreeMap::init(pool_own_sources_memory()));

    // The finishes by their source transaction: what the finish rule looks up instead of
    // walking every flow. Part of the fold: only `Flow` lines write it and the next.
    static FLOW_BY_SOURCE: RefCell<StableBTreeMap<types::TxHash, QuoteHash, Memory>> =
        RefCell::new(StableBTreeMap::init(flow_by_source_memory()));

    // The flows by their own transaction: a CCTP finish found from the Base transaction a
    // pool arrival names.
    static FLOW_BY_TX: RefCell<StableBTreeMap<types::TxHash, QuoteHash, Memory>> =
        RefCell::new(StableBTreeMap::init(flow_by_tx_memory()));

    // Every controller's sweep by id, never removed: what each took off which lines, and
    // whether it was taken back. Part of the fold: only `Swept` and `SweepReverted` write it.
    static SWEEPS: RefCell<StableBTreeMap<u64, types::sweep::SweepEntry, Memory>> =
        RefCell::new(StableBTreeMap::init(sweeps_memory()));

    // The Eco rescues by order id (P11e): the line that signed each.
    static ECO_RESCUES: RefCell<StableBTreeMap<PoolRef, u64, Memory>> =
        RefCell::new(StableBTreeMap::init(eco_rescues_memory()));
}

/// The fold in stable memory. Only this module can build one, so `append_event` is its one
/// and only writer.
pub struct StableStore(());

impl Store for StableStore {
    fn flow(&self, quote_hash: &QuoteHash) -> Option<Flow> {
        FLOWS.with(|flows| flows.borrow().get(quote_hash))
    }

    fn put_flow(&mut self, quote_hash: QuoteHash, flow: Flow) {
        // the two indexes the flow rules read, kept with every write of a flow
        if let Some(tx) = flow.source_tx {
            FLOW_BY_SOURCE.with(|index| index.borrow_mut().insert(tx, quote_hash));
        }
        if let Some(tx) = flow.tx_hash {
            FLOW_BY_TX.with(|index| index.borrow_mut().insert(tx, quote_hash));
        }
        FLOWS.with(|flows| flows.borrow_mut().insert(quote_hash, flow));
    }

    fn flows(&self) -> Vec<(QuoteHash, Flow)> {
        FLOWS.with(|flows| flows.borrow().iter().collect())
    }

    fn meta(&self) -> LedgerMeta {
        LEDGER_META.with(|meta| *meta.borrow().get())
    }

    fn put_meta(&mut self, meta: LedgerMeta) {
        // out of stable memory is not a caller error, so it traps, rolling back the message
        LEDGER_META.with(|cell| cell.borrow_mut().set(meta).expect("ledger meta cell write"));
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

    fn pool_move(&self, id: u64) -> Option<types::pool::Move> {
        POOL_MOVES.with(|moves| moves.borrow().get(&id))
    }

    fn put_pool_move(&mut self, id: u64, item: types::pool::Move) {
        POOL_MOVES.with(|moves| moves.borrow_mut().insert(id, item));
    }

    fn pool_moves(&self) -> Vec<(u64, types::pool::Move)> {
        POOL_MOVES.with(|moves| moves.borrow().iter().collect())
    }

    fn pool_own_source(&self, tx: &types::TxHash) -> Option<types::pool::OwnSource> {
        POOL_OWN_SOURCES.with(|sources| sources.borrow().get(tx))
    }

    fn put_pool_own_source(&mut self, tx: types::TxHash, source: types::pool::OwnSource) {
        POOL_OWN_SOURCES.with(|sources| sources.borrow_mut().insert(tx, source));
    }

    fn pool_own_sources(&self) -> Vec<(types::TxHash, types::pool::OwnSource)> {
        POOL_OWN_SOURCES.with(|sources| sources.borrow().iter().collect())
    }

    fn flow_by_source(&self, tx: &types::TxHash) -> Option<QuoteHash> {
        FLOW_BY_SOURCE.with(|index| index.borrow().get(tx))
    }

    fn flow_by_tx(&self, tx: &types::TxHash) -> Option<QuoteHash> {
        FLOW_BY_TX.with(|index| index.borrow().get(tx))
    }

    fn sweep(&self, id: u64) -> Option<types::sweep::SweepEntry> {
        SWEEPS.with(|sweeps| sweeps.borrow().get(&id))
    }

    fn put_sweep(&mut self, id: u64, entry: types::sweep::SweepEntry) {
        SWEEPS.with(|sweeps| sweeps.borrow_mut().insert(id, entry));
    }

    fn sweeps(&self) -> Vec<(u64, types::sweep::SweepEntry)> {
        SWEEPS.with(|sweeps| sweeps.borrow().iter().collect())
    }

    fn sweep_count(&self) -> u64 {
        SWEEPS.with(|sweeps| sweeps.borrow().len())
    }

    fn eco_rescue(&self, order_id: &PoolRef) -> Option<u64> {
        ECO_RESCUES.with(|rescues| rescues.borrow().get(order_id))
    }

    fn put_eco_rescue(&mut self, order_id: PoolRef, index: u64) {
        ECO_RESCUES.with(|rescues| rescues.borrow_mut().insert(order_id, index));
    }

    fn eco_rescues(&self) -> Vec<(PoolRef, u64)> {
        ECO_RESCUES.with(|rescues| rescues.borrow().iter().collect())
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
}

/// Why an event was not appended. Nothing was written.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum AppendError {
    #[error("log is at {log_len} but the event would be written as {sealed}")]
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

/// Why the stable fold is out of step with the log it is the fold of: the index it writes
/// next is not the log's length.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("the log holds {log_len} events but the fold expects to write event {next_event_index}")]
pub struct FoldOutOfStep {
    pub log_len: u64,
    pub next_event_index: EventIndex,
}

impl From<FoldOutOfStep> for AppendError {
    fn from(error: FoldOutOfStep) -> Self {
        Self::IndexMismatch {
            log_len: error.log_len,
            sealed: error.next_event_index,
        }
    }
}

impl From<AppendError> for settlement_api::types::errors::AppendError {
    fn from(error: AppendError) -> Self {
        match error {
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
    LEDGER_META.with(|_| ());
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
    POOL_MOVES.with(|_| ());
    POOL_OWN_SOURCES.with(|_| ());
    ECO_RESCUES.with(|_| ());
    SWEEPS.with(|_| ());
    FLOW_BY_SOURCE.with(|_| ());
    FLOW_BY_TX.with(|_| ());
}

/// The one and only path that writes an event, at the canister's clock.
pub fn append_event(payload: EventType) -> Result<EventIndex, AppendError> {
    append_event_at(payload, Timestamp::from_nanos(ic_cdk::api::time()))
}

/// In O(1), whether the fold is in step with its log: the index it writes next is the log's
/// length. `post_upgrade` refuses an upgrade on it, and `append_event` refuses to write on
/// it.
pub fn ensure_fold_in_step() -> Result<(), FoldOutOfStep> {
    let meta = StableStore(()).meta();
    let log_len = EVENTS.with(|e| e.borrow().len());
    if meta.next_event_index.get() != log_len {
        return Err(FoldOutOfStep {
            log_len,
            next_event_index: meta.next_event_index,
        });
    }
    Ok(())
}

/// [`append_event`] at `timestamp`: a caller that already read the clock writes on the same
/// reading, and a unit test runs without a canister. Guard, append, apply, all within a
/// single message, so a trap anywhere rolls back the log and the fold together.
pub fn append_event_at(
    payload: EventType,
    timestamp: Timestamp,
) -> Result<EventIndex, AppendError> {
    // an index off the log's length would write one slot and apply another
    ensure_fold_in_step()?;
    let mut state = State::new(StableStore(()));
    state.check_at(&payload, timestamp)?;
    let meta = state.meta();
    let index = meta.next_event_index;
    if index.next().is_none() {
        return Err(AppendError::LogFull);
    }
    let event = Event {
        index,
        timestamp,
        payload,
    };
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
    // arms one coalesced drive, and every pool line arms the backstop, which comes back
    // while work is open. A trap later in this message rolls the timers back with the line.
    if event.payload.moves_a_step() {
        crate::engine::arm();
    }
    if matches!(event.payload, EventType::Pool { .. }) {
        crate::task_manager::backstop::arm();
    }
    Ok(index)
}

/// A balance read of `chain_id` at `block` that passed, so no line records it: the fees
/// and the pool's arrivals booked at or below `block` leave the unseen lists, as a
/// `Checked` line would drop them. The one write to the fold that is not a line's. It
/// changes no answer for a read at `block` or above, and the check never reads below the
/// block it last read at (`LastRead::floor`).
pub fn forget_seen(chain_id: ChainId, block: types::BlockNumber) {
    State::new(StableStore(())).seen_through(chain_id, block);
}

/// Read-only view of the fold. There is deliberately no mutable twin.
pub fn read_state<R>(f: impl FnOnce(&State<StableStore>) -> R) -> R {
    f(&State::new(StableStore(())))
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

/// Whether folding the log onto the heap reproduces the stable fold exactly: the tests'
/// check that every write went through `append_event`.
#[cfg(test)]
pub fn verify_replay() -> bool {
    let log: Vec<Event> = EVENTS.with(|e| e.borrow().iter().collect());
    match crate::state::transitions::replay(log) {
        Ok(rebuilt) => read_state(|live| rebuilt.matches(live)),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests;
