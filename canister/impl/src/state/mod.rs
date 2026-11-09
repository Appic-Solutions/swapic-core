#[cfg(test)]
use std::collections::{BTreeMap, BTreeSet};
use std::ops::RangeFrom;
use types::books::{BookStep, Books, UserFeeKey};
use types::events::TxPurpose;
use types::hash::{OwnerKey, PoolRef};
use types::pool::{
    CreditKey, EvmPayout, NonceUse, PoolLedger, PoolLookup, PoolStep, Registered, ShareLine, Shares,
};
use types::sweep::SweepEntry;
use types::{
    BlockNumber, ChainId, EventIndex, Flow, LedgerMeta, Nonce, NonceKey, QuoteHash, Timestamp,
    TokenAmount, TxHash, UnsignedTx,
};

pub mod transitions;

/// Where the event log's fold lives. [`State`] reads and writes only through this, so one
/// copy of the transition rules serves every store.
pub trait Store {
    fn flow(&self, quote_hash: &QuoteHash) -> Option<Flow>;
    fn put_flow(&mut self, quote_hash: QuoteHash, flow: Flow);
    /// Every flow, in quote hash order.
    fn flows(&self) -> Vec<(QuoteHash, Flow)>;
    /// The finish a source transaction was finished under, as last indexed: a reader checks
    /// the flow still names it.
    fn flow_by_source(&self, tx: &TxHash) -> Option<QuoteHash>;
    /// The flow one of whose transactions is `tx`: its first, a replacement, or the one that
    /// landed.
    fn flow_by_tx(&self, tx: &TxHash) -> Option<QuoteHash>;
    /// Indexes `tx` as one of `quote_hash`'s transactions.
    fn put_flow_tx(&mut self, tx: TxHash, quote_hash: QuoteHash);
    fn meta(&self) -> LedgerMeta;
    fn put_meta(&mut self, meta: LedgerMeta);
    /// The number the next transaction on `chain_id` must carry. A chain nothing was ever
    /// sent on is at zero: the allocator counts the transactions this canister created and
    /// never reads a chain to decide (rule A4).
    fn next_nonce(&self, chain_id: &ChainId) -> Nonce;
    fn put_next_nonce(&mut self, chain_id: ChainId, nonce: Nonce);
    /// Every chain the allocator has handed out a nonce on, in chain id order.
    fn nonces(&self) -> Vec<(ChainId, Nonce)>;
    fn book(&self, chain_id: &ChainId) -> Option<Books>;
    fn put_book(&mut self, chain_id: ChainId, books: Books);
    /// Every chain's books, in chain id order.
    fn books(&self) -> Vec<(ChainId, Books)>;
    /// Whether the user-paid fees of this transaction are booked.
    fn is_user_fee_booked(&self, key: &UserFeeKey) -> bool;
    fn put_user_fee_booked(&mut self, key: UserFeeKey);
    /// Every transaction whose user-paid fees are booked, in key order.
    fn booked_user_fees(&self) -> Vec<UserFeeKey>;
    /// The pool's ledger, empty before its first step.
    fn pool_ledger(&self) -> PoolLedger;
    fn put_pool_ledger(&mut self, ledger: PoolLedger);
    fn pool_deposit(&self, r#ref: &PoolRef) -> Option<Registered>;
    fn put_pool_deposit(&mut self, r#ref: PoolRef, deposit: Registered);
    /// Every pool deposit, in ref order: what the audit compares, never what a timer reads.
    fn pool_deposits(&self) -> Vec<(PoolRef, Registered)>;
    fn pool_share(&self, owner: &OwnerKey) -> Option<ShareLine>;
    fn put_pool_share(&mut self, owner: OwnerKey, line: ShareLine);
    /// Every share line, in key order.
    fn pool_shares(&self) -> Vec<(OwnerKey, ShareLine)>;
    fn is_pool_credited(&self, key: &CreditKey) -> bool;
    fn put_pool_credited(&mut self, key: CreditKey);
    /// Every credited transfer, in key order.
    fn pool_credited(&self) -> Vec<CreditKey>;
    /// A payout from the Base pocket, by id.
    fn pool_payout(&self, id: u64) -> Option<EvmPayout>;
    fn put_pool_payout(&mut self, id: u64, payout: EvmPayout);
    /// Every Base payout, in id order: what the audit compares.
    fn pool_payouts(&self) -> Vec<(u64, EvmPayout)>;
    /// The Base queue: a payout at its place.
    fn put_payout_queued(&mut self, seq: u64, id: u64);
    fn remove_payout_queued(&mut self, seq: u64);
    /// The first `limit` places of the Base queue, in order.
    fn payout_queue(&self, limit: usize) -> Vec<(u64, u64)>;
    fn is_withdrawal_nonce_used(&self, key: &NonceUse) -> bool;
    fn put_withdrawal_nonce(&mut self, key: NonceUse);
    /// Every withdrawal nonce used, in staker then nonce order.
    fn withdrawal_nonces(&self) -> Vec<NonceUse>;
    /// The paid payout whose order published the Eco intent `intent`.
    fn payout_by_intent(&self, intent: &PoolRef) -> Option<u64>;
    fn put_payout_intent(&mut self, intent: PoolRef, id: u64);
    /// Every paid payout's Eco intent, in intent order.
    fn payout_intents(&self) -> Vec<(PoolRef, u64)>;
    /// An orphan, by its credit key.
    fn pool_orphan(&self, key: &CreditKey) -> Option<types::pool::Orphan>;
    fn put_pool_orphan(&mut self, key: CreditKey, orphan: types::pool::Orphan);
    /// Every orphan, in key order.
    fn pool_orphans(&self) -> Vec<(CreditKey, types::pool::Orphan)>;
    /// A rebalance move, by id.
    fn pool_move(&self, id: u64) -> Option<types::pool::Move>;
    fn put_pool_move(&mut self, id: u64, item: types::pool::Move);
    /// Every move, in id order: what the audit compares.
    fn pool_moves(&self) -> Vec<(u64, types::pool::Move)>;
    /// The canister's own transfer a source transaction is, if it is one.
    fn pool_own_source(&self, tx: &TxHash) -> Option<types::pool::OwnSource>;
    fn put_pool_own_source(&mut self, tx: TxHash, source: types::pool::OwnSource);
    /// Every own source, in hash order: what the audit compares.
    fn pool_own_sources(&self) -> Vec<(TxHash, types::pool::OwnSource)>;
    /// The line that signed the rescue of the intent an order published, if one did.
    fn eco_rescue(&self, order_id: &PoolRef) -> Option<u64>;
    fn put_eco_rescue(&mut self, order_id: PoolRef, index: u64);
    /// Every rescue, in order id order: what the audit compares.
    fn eco_rescues(&self) -> Vec<(PoolRef, u64)>;
    /// A controller's sweep, by id.
    fn sweep(&self, id: u64) -> Option<SweepEntry>;
    fn put_sweep(&mut self, id: u64, entry: SweepEntry);
    /// Every sweep, in id order.
    fn sweeps(&self) -> Vec<(u64, SweepEntry)>;
    /// How many sweeps were ever made: the next one's id.
    fn sweep_count(&self) -> u64;
    /// Indexes a line that records a signed transaction's bytes.
    fn put_signed(&mut self, index: EventIndex);
    /// At most `limit` of the indexed lines in `range`, in log order.
    fn signed_in(&self, range: RangeFrom<EventIndex>, limit: usize) -> Vec<EventIndex>;
    /// The nonce `TxCreated` handed out at `key` and no signed record or `TxCancelled` has
    /// ended, if there is one.
    fn unsigned_nonce(&self, key: &NonceKey) -> Option<UnsignedTx>;
    fn put_unsigned_nonce(&mut self, key: NonceKey, unsigned: UnsignedTx);
    fn remove_unsigned_nonce(&mut self, key: &NonceKey);
    /// Every nonce still waiting for its signed record, in chain then nonce order. The
    /// collection holds one entry per send between its `TxCreated` and its signed record, plus
    /// the allocations whose transaction never came back, and the outbox pass ends those
    /// within a batch window: it is small by construction, so a walk of it is the read.
    fn unsigned_nonces(&self) -> Vec<(NonceKey, UnsignedTx)>;

    /// Whether `quote_hash` already holds a nonce it has not signed for. Rule A4 read from
    /// the key's side: one key never has two numbers out at once, so two sends that
    /// interleave at the signature cannot both allocate.
    fn has_unsigned_nonce_for(&self, quote_hash: &QuoteHash) -> bool {
        self.unsigned_nonces()
            .iter()
            .any(|(_, unsigned)| unsigned.purpose.quote_hash().as_ref() == Some(quote_hash))
    }

    /// The nonce `quote_hash` holds unsigned, if it holds one: what a late signature is
    /// checked against before its record is written.
    fn unsigned_nonce_of(&self, quote_hash: &QuoteHash) -> Option<NonceKey> {
        self.unsigned_nonces()
            .into_iter()
            .find(|(_, unsigned)| unsigned.purpose.quote_hash().as_ref() == Some(quote_hash))
            .map(|(key, _)| key)
    }
}

/// The fold as the pool's rules read it (`types::pool::PoolLookup`).
pub struct PoolView<'a, S>(&'a S);

impl<S: Store> PoolLookup for PoolView<'_, S> {
    fn deposit(&self, r#ref: &PoolRef) -> Option<Registered> {
        self.0.pool_deposit(r#ref)
    }

    fn shares_of(&self, owner: &OwnerKey) -> Shares {
        self.0
            .pool_share(owner)
            .map_or(Shares::ZERO, |line| line.shares)
    }

    fn is_credited(&self, key: &CreditKey) -> bool {
        self.0.is_pool_credited(key)
    }

    fn payout(&self, id: u64) -> Option<EvmPayout> {
        self.0.pool_payout(id)
    }

    fn queue_head(&self) -> Option<(u64, u64)> {
        self.0.payout_queue(1).into_iter().next()
    }

    fn nonce_used(&self, key: &NonceUse) -> bool {
        self.0.is_withdrawal_nonce_used(key)
    }

    fn orphan(&self, key: &CreditKey) -> Option<types::pool::Orphan> {
        self.0.pool_orphan(key)
    }

    fn eco_rescued(&self, order_id: &[u8; 32]) -> bool {
        self.0.eco_rescue(&PoolRef::new(*order_id)).is_some()
    }

    fn pool_move(&self, id: u64) -> Option<types::pool::Move> {
        self.0.pool_move(id)
    }
}

/// A [`Store`] on the heap: what unit tests fold into.
#[cfg(test)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MemoryStore {
    meta: LedgerMeta,
    nonces: BTreeMap<ChainId, Nonce>,
    unsigned: BTreeMap<NonceKey, UnsignedTx>,
    flows: BTreeMap<QuoteHash, Flow>,
    signed: BTreeSet<EventIndex>,
    books: BTreeMap<ChainId, Books>,
    pool_ledger: PoolLedger,
    pool_deposits: BTreeMap<PoolRef, Registered>,
    pool_shares: BTreeMap<OwnerKey, ShareLine>,
    pool_credited: BTreeSet<CreditKey>,
    pool_payouts: BTreeMap<u64, EvmPayout>,
    payout_queue: BTreeMap<u64, u64>,
    withdrawal_nonces: BTreeSet<NonceUse>,
    payout_intents: BTreeMap<PoolRef, u64>,
    pool_orphans: BTreeMap<CreditKey, types::pool::Orphan>,
    pool_moves: BTreeMap<u64, types::pool::Move>,
    pool_own_sources: BTreeMap<TxHash, types::pool::OwnSource>,
    eco_rescues: BTreeMap<PoolRef, u64>,
    sweeps: BTreeMap<u64, SweepEntry>,
    flow_by_source: BTreeMap<TxHash, QuoteHash>,
    flow_by_tx: BTreeMap<TxHash, QuoteHash>,
    booked_user_fees: BTreeSet<UserFeeKey>,
}

#[cfg(test)]
impl Store for MemoryStore {
    fn flow_by_source(&self, tx: &TxHash) -> Option<QuoteHash> {
        self.flow_by_source.get(tx).copied()
    }

    fn flow_by_tx(&self, tx: &TxHash) -> Option<QuoteHash> {
        self.flow_by_tx.get(tx).copied()
    }

    fn put_flow_tx(&mut self, tx: TxHash, quote_hash: QuoteHash) {
        self.flow_by_tx.insert(tx, quote_hash);
    }

    fn sweep(&self, id: u64) -> Option<SweepEntry> {
        self.sweeps.get(&id).copied()
    }

    fn put_sweep(&mut self, id: u64, entry: SweepEntry) {
        self.sweeps.insert(id, entry);
    }

    fn sweeps(&self) -> Vec<(u64, SweepEntry)> {
        self.sweeps
            .iter()
            .map(|(id, entry)| (*id, *entry))
            .collect()
    }

    fn sweep_count(&self) -> u64 {
        self.sweeps.len() as u64
    }

    fn flow(&self, quote_hash: &QuoteHash) -> Option<Flow> {
        self.flows.get(quote_hash).cloned()
    }

    fn put_flow(&mut self, quote_hash: QuoteHash, flow: Flow) {
        if let Some(tx) = flow.source_tx {
            self.flow_by_source.insert(tx, quote_hash);
        }
        if let Some(tx) = flow.tx_hash {
            self.flow_by_tx.insert(tx, quote_hash);
        }
        self.flows.insert(quote_hash, flow);
    }

    fn flows(&self) -> Vec<(QuoteHash, Flow)> {
        self.flows
            .iter()
            .map(|(hash, flow)| (*hash, flow.clone()))
            .collect()
    }

    fn meta(&self) -> LedgerMeta {
        self.meta
    }

    fn put_meta(&mut self, meta: LedgerMeta) {
        self.meta = meta;
    }

    fn next_nonce(&self, chain_id: &ChainId) -> Nonce {
        self.nonces.get(chain_id).copied().unwrap_or(Nonce::ZERO)
    }

    fn put_next_nonce(&mut self, chain_id: ChainId, nonce: Nonce) {
        self.nonces.insert(chain_id, nonce);
    }

    fn nonces(&self) -> Vec<(ChainId, Nonce)> {
        self.nonces
            .iter()
            .map(|(chain, nonce)| (*chain, *nonce))
            .collect()
    }

    fn put_signed(&mut self, index: EventIndex) {
        self.signed.insert(index);
    }

    fn book(&self, chain_id: &ChainId) -> Option<Books> {
        self.books.get(chain_id).cloned()
    }

    fn put_book(&mut self, chain_id: ChainId, books: Books) {
        self.books.insert(chain_id, books);
    }

    fn books(&self) -> Vec<(ChainId, Books)> {
        self.books
            .iter()
            .map(|(chain, books)| (*chain, books.clone()))
            .collect()
    }

    fn is_user_fee_booked(&self, key: &UserFeeKey) -> bool {
        self.booked_user_fees.contains(key)
    }

    fn put_user_fee_booked(&mut self, key: UserFeeKey) {
        self.booked_user_fees.insert(key);
    }

    fn booked_user_fees(&self) -> Vec<UserFeeKey> {
        self.booked_user_fees.iter().copied().collect()
    }

    fn signed_in(&self, range: RangeFrom<EventIndex>, limit: usize) -> Vec<EventIndex> {
        self.signed.range(range).take(limit).copied().collect()
    }

    fn pool_ledger(&self) -> PoolLedger {
        self.pool_ledger.clone()
    }

    fn put_pool_ledger(&mut self, ledger: PoolLedger) {
        self.pool_ledger = ledger;
    }

    fn pool_deposit(&self, r#ref: &PoolRef) -> Option<Registered> {
        self.pool_deposits.get(r#ref).cloned()
    }

    fn put_pool_deposit(&mut self, r#ref: PoolRef, deposit: Registered) {
        self.pool_deposits.insert(r#ref, deposit);
    }

    fn pool_deposits(&self) -> Vec<(PoolRef, Registered)> {
        self.pool_deposits
            .iter()
            .map(|(r#ref, deposit)| (*r#ref, deposit.clone()))
            .collect()
    }

    fn pool_share(&self, owner: &OwnerKey) -> Option<ShareLine> {
        self.pool_shares.get(owner).copied()
    }

    fn put_pool_share(&mut self, owner: OwnerKey, line: ShareLine) {
        self.pool_shares.insert(owner, line);
    }

    fn pool_shares(&self) -> Vec<(OwnerKey, ShareLine)> {
        self.pool_shares
            .iter()
            .map(|(key, line)| (*key, *line))
            .collect()
    }

    fn is_pool_credited(&self, key: &CreditKey) -> bool {
        self.pool_credited.contains(key)
    }

    fn put_pool_credited(&mut self, key: CreditKey) {
        self.pool_credited.insert(key);
    }

    fn pool_credited(&self) -> Vec<CreditKey> {
        self.pool_credited.iter().copied().collect()
    }

    fn pool_payout(&self, id: u64) -> Option<EvmPayout> {
        self.pool_payouts.get(&id).cloned()
    }

    fn put_pool_payout(&mut self, id: u64, payout: EvmPayout) {
        self.pool_payouts.insert(id, payout);
    }

    fn pool_payouts(&self) -> Vec<(u64, EvmPayout)> {
        self.pool_payouts
            .iter()
            .map(|(id, payout)| (*id, payout.clone()))
            .collect()
    }

    fn put_payout_queued(&mut self, seq: u64, id: u64) {
        self.payout_queue.insert(seq, id);
    }

    fn remove_payout_queued(&mut self, seq: u64) {
        self.payout_queue.remove(&seq);
    }

    fn payout_queue(&self, limit: usize) -> Vec<(u64, u64)> {
        self.payout_queue
            .iter()
            .take(limit)
            .map(|(seq, id)| (*seq, *id))
            .collect()
    }

    fn is_withdrawal_nonce_used(&self, key: &NonceUse) -> bool {
        self.withdrawal_nonces.contains(key)
    }

    fn put_withdrawal_nonce(&mut self, key: NonceUse) {
        self.withdrawal_nonces.insert(key);
    }

    fn withdrawal_nonces(&self) -> Vec<NonceUse> {
        self.withdrawal_nonces.iter().copied().collect()
    }

    fn payout_by_intent(&self, intent: &PoolRef) -> Option<u64> {
        self.payout_intents.get(intent).copied()
    }

    fn put_payout_intent(&mut self, intent: PoolRef, id: u64) {
        self.payout_intents.insert(intent, id);
    }

    fn payout_intents(&self) -> Vec<(PoolRef, u64)> {
        self.payout_intents
            .iter()
            .map(|(intent, id)| (*intent, *id))
            .collect()
    }

    fn pool_orphan(&self, key: &CreditKey) -> Option<types::pool::Orphan> {
        self.pool_orphans.get(key).cloned()
    }

    fn put_pool_orphan(&mut self, key: CreditKey, orphan: types::pool::Orphan) {
        self.pool_orphans.insert(key, orphan);
    }

    fn pool_orphans(&self) -> Vec<(CreditKey, types::pool::Orphan)> {
        self.pool_orphans
            .iter()
            .map(|(key, orphan)| (*key, orphan.clone()))
            .collect()
    }

    fn pool_move(&self, id: u64) -> Option<types::pool::Move> {
        self.pool_moves.get(&id).cloned()
    }

    fn put_pool_move(&mut self, id: u64, item: types::pool::Move) {
        self.pool_moves.insert(id, item);
    }

    fn pool_moves(&self) -> Vec<(u64, types::pool::Move)> {
        self.pool_moves
            .iter()
            .map(|(id, item)| (*id, item.clone()))
            .collect()
    }

    fn pool_own_source(&self, tx: &TxHash) -> Option<types::pool::OwnSource> {
        self.pool_own_sources.get(tx).copied()
    }

    fn put_pool_own_source(&mut self, tx: TxHash, source: types::pool::OwnSource) {
        self.pool_own_sources.insert(tx, source);
    }

    fn pool_own_sources(&self) -> Vec<(TxHash, types::pool::OwnSource)> {
        self.pool_own_sources
            .iter()
            .map(|(tx, source)| (*tx, *source))
            .collect()
    }

    fn eco_rescue(&self, order_id: &PoolRef) -> Option<u64> {
        self.eco_rescues.get(order_id).copied()
    }

    fn put_eco_rescue(&mut self, order_id: PoolRef, index: u64) {
        self.eco_rescues.insert(order_id, index);
    }

    fn eco_rescues(&self) -> Vec<(PoolRef, u64)> {
        self.eco_rescues
            .iter()
            .map(|(order_id, index)| (*order_id, *index))
            .collect()
    }

    fn unsigned_nonce(&self, key: &NonceKey) -> Option<UnsignedTx> {
        self.unsigned.get(key).copied()
    }

    fn put_unsigned_nonce(&mut self, key: NonceKey, unsigned: UnsignedTx) {
        self.unsigned.insert(key, unsigned);
    }

    fn remove_unsigned_nonce(&mut self, key: &NonceKey) {
        self.unsigned.remove(key);
    }

    fn unsigned_nonces(&self) -> Vec<(NonceKey, UnsignedTx)> {
        self.unsigned
            .iter()
            .map(|(key, unsigned)| (*key, *unsigned))
            .collect()
    }
}

/// The fold of the event log. [`State::check`] decides whether an event may happen, and
/// [`transitions::apply_state_transition`] records one that did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct State<S> {
    store: S,
}

#[cfg(test)]
impl State<MemoryStore> {
    /// Whether `other` holds exactly this heap fold, whatever its store. The destructure has
    /// no `..`, so a collection added to the store fails to compile until it is compared.
    pub fn matches<T: Store>(&self, other: &State<T>) -> bool {
        let MemoryStore {
            meta,
            nonces,
            unsigned,
            flows,
            signed,
            books,
            pool_ledger,
            pool_deposits,
            pool_shares,
            pool_credited,
            pool_payouts,
            payout_queue,
            withdrawal_nonces,
            payout_intents,
            pool_orphans,
            pool_moves,
            pool_own_sources,
            eco_rescues,
            sweeps,
            flow_by_source: _,
            flow_by_tx: _,
            booked_user_fees,
        } = &self.store;
        let other_flows = other.store.flows();
        let other_nonces = other.store.nonces();
        let other_unsigned = other.store.unsigned_nonces();
        *meta == other.meta()
            && nonces
                .iter()
                .eq(other_nonces.iter().map(|(chain, nonce)| (chain, nonce)))
            && unsigned
                .iter()
                .eq(other_unsigned.iter().map(|(key, tx)| (key, tx)))
            && flows
                .iter()
                .eq(other_flows.iter().map(|(hash, flow)| (hash, flow)))
            && signed
                .iter()
                .eq(other.store.signed_in(EventIndex::ZERO.., usize::MAX).iter())
            && books.iter().eq(other
                .store
                .books()
                .iter()
                .map(|(chain, books)| (chain, books)))
            && *pool_ledger == other.store.pool_ledger()
            && pool_deposits.iter().eq(other
                .store
                .pool_deposits()
                .iter()
                .map(|(r#ref, deposit)| (r#ref, deposit)))
            && pool_shares.iter().eq(other
                .store
                .pool_shares()
                .iter()
                .map(|(key, line)| (key, line)))
            && pool_credited.iter().eq(other.store.pool_credited().iter())
            && booked_user_fees
                .iter()
                .eq(other.store.booked_user_fees().iter())
            && pool_payouts.iter().eq(other
                .store
                .pool_payouts()
                .iter()
                .map(|(id, payout)| (id, payout)))
            && payout_queue.iter().eq(other
                .store
                .payout_queue(usize::MAX)
                .iter()
                .map(|(seq, id)| (seq, id)))
            && withdrawal_nonces
                .iter()
                .eq(other.store.withdrawal_nonces().iter())
            && payout_intents.iter().eq(other
                .store
                .payout_intents()
                .iter()
                .map(|(intent, id)| (intent, id)))
            && pool_orphans.iter().eq(other
                .store
                .pool_orphans()
                .iter()
                .map(|(key, orphan)| (key, orphan)))
            && pool_moves
                .iter()
                .eq(other.store.pool_moves().iter().map(|(id, item)| (id, item)))
            && pool_own_sources.iter().eq(other
                .store
                .pool_own_sources()
                .iter()
                .map(|(tx, source)| (tx, source)))
            && eco_rescues.iter().eq(other
                .store
                .eco_rescues()
                .iter()
                .map(|(order_id, index)| (order_id, index)))
            && sweeps
                .iter()
                .eq(other.store.sweeps().iter().map(|(id, entry)| (id, entry)))
    }
}

impl<S: Store> State<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }

    pub fn store(&self) -> &S {
        &self.store
    }

    /// The fold as the bare store, for saving it.
    pub fn into_store(self) -> S {
        self.store
    }

    pub fn meta(&self) -> LedgerMeta {
        self.store.meta()
    }

    /// The number the next transaction on `chain_id` must carry.
    pub fn next_nonce(&self, chain_id: &ChainId) -> Nonce {
        self.store.next_nonce(chain_id)
    }

    /// Every nonce that has been allocated and not yet signed for, in chain then nonce
    /// order. What the pass that keeps rule A5 reads.
    pub fn unsigned_nonces(&self) -> Vec<(NonceKey, UnsignedTx)> {
        self.store.unsigned_nonces()
    }

    /// A chain's books, empty where nothing was booked yet.
    pub fn book(&self, chain_id: &ChainId) -> Books {
        self.store.book(chain_id).unwrap_or_default()
    }

    /// A balance read of `chain_id` at `block`: the fees and the pool's arrivals booked at
    /// or below it are in that balance, so neither list keeps them.
    pub(crate) fn seen_through(&mut self, chain_id: ChainId, block: BlockNumber) {
        let books = self.book(&chain_id).seen_through(block);
        self.store.put_book(chain_id, books);
        let pool = self.store.pool_ledger().seen_through(chain_id, block);
        self.store.put_pool_ledger(pool);
    }

    /// A step of a chain's books, sealed at `at`. A shortfall latches the pool's pause,
    /// and nothing else; a controller pauses the pool or reopens it, which also clears a
    /// latched shortfall it has looked at.
    fn record_books(&mut self, chain_id: ChainId, step: &BookStep, at: Timestamp) {
        let books = self.book(&chain_id).apply(step);
        self.store.put_book(chain_id, books);
        if let BookStep::UserFeeEarned { tx_hash, .. } = step {
            self.store.put_user_fee_booked(UserFeeKey {
                chain: chain_id,
                tx: *tx_hash,
            });
        }
        // a controller's reopening clears the ICP latch with the Base one
        match step {
            BookStep::Checked { check, .. } => self.seen_through(chain_id, check.block),
            BookStep::PoolPaused { paused: false } => {
                let pool = self.store.pool_ledger().reopened();
                self.store.put_pool_ledger(pool);
            }
            _ => {}
        }
        let meta = self.store.meta();
        match step {
            BookStep::Checked {
                shortfall: true, ..
            } => self.store.put_meta(LedgerMeta {
                pool_paused_by: Some(chain_id),
                ..meta
            }),
            BookStep::PoolPaused { paused: true } => self.store.put_meta(LedgerMeta {
                pool_paused_at: Some(at),
                ..meta
            }),
            BookStep::PoolPaused { paused: false } => self.store.put_meta(LedgerMeta {
                pool_paused_at: None,
                pool_paused_by: None,
                ..meta
            }),
            _ => {}
        }
    }

    /// A controller's sweep, recorded: its amount off the lines it came from.
    pub(crate) fn record_sweep(&mut self, sweep: &types::sweep::Sweep) {
        self.shift_sweep(sweep, false);
        self.store.put_sweep(
            sweep.id,
            SweepEntry {
                sweep: *sweep,
                reverted: false,
            },
        );
    }

    /// A sweep that moved nothing: its amount back on the lines it came from.
    pub(crate) fn record_sweep_reverted(&mut self, id: u64) {
        let Some(mut entry) = self.store.sweep(id) else {
            return;
        };
        self.shift_sweep(&entry.sweep, true);
        entry.reverted = true;
        self.store.put_sweep(id, entry);
    }

    /// Moves a sweep's parts off their lines, or `back` onto them.
    fn shift_sweep(&mut self, sweep: &types::sweep::Sweep, back: bool) {
        let shift = |line: TokenAmount, by: TokenAmount| {
            if back {
                line.checked_add(by).unwrap_or(TokenAmount::MAX)
            } else {
                line.checked_sub(by).unwrap_or(TokenAmount::ZERO)
            }
        };
        if let Some(chain) = sweep.chain() {
            if sweep.from_fee_income > TokenAmount::ZERO {
                let mut books = self.book(&chain);
                books.fee_income = shift(books.fee_income, sweep.from_fee_income);
                self.store.put_book(chain, books);
            }
        }
        if sweep.from_pool_fees > TokenAmount::ZERO {
            let mut ledger = self.store.pool_ledger();
            let mut lines = ledger.lines();
            match sweep.asset {
                types::sweep::SweepAsset::FeeUsdc => {
                    lines.fees_base = shift(lines.fees_base, sweep.from_pool_fees)
                }
                types::sweep::SweepAsset::CkUsdcFees => {
                    lines.fees_icp = shift(lines.fees_icp, sweep.from_pool_fees)
                }
                _ => {}
            }
            ledger.payouts = Some(lines);
            self.store.put_pool_ledger(ledger);
        }
    }

    /// The pool's ledger.
    pub fn pool(&self) -> PoolLedger {
        self.store.pool_ledger()
    }

    /// The fold as the pool's rules read it.
    pub fn pool_view(&self) -> PoolView<'_, S> {
        PoolView(&self.store)
    }

    /// A pool step the guard admitted, sealed at `at`: the ledger and the entries of the maps
    /// beside it, all from the one step.
    fn record_pool(&mut self, step: &PoolStep, at: types::Timestamp, index: EventIndex) {
        let effects =
            self.store
                .pool_ledger()
                .apply_at(step, &PoolView(&self.store), at, index.get());
        self.store.put_pool_ledger(effects.ledger);
        if let Some((r#ref, deposit)) = effects.deposit {
            self.store.put_pool_deposit(r#ref, deposit);
        }
        if let Some((key, line)) = effects.shares {
            self.store.put_pool_share(key, line);
        }
        if let Some(key) = effects.credited {
            self.store.put_pool_credited(key);
        }
        if let Some(payout) = effects.payout {
            self.store.put_pool_payout(payout.id, payout);
        }
        if let Some(seq) = effects.dequeued {
            self.store.remove_payout_queued(seq);
        }
        if let Some((seq, id)) = effects.queued {
            self.store.put_payout_queued(seq, id);
        }
        if let Some(key) = effects.nonce {
            self.store.put_withdrawal_nonce(key);
        }
        if let Some((intent, id)) = effects.intent {
            self.store.put_payout_intent(PoolRef::new(intent), id);
        }
        if let Some((key, orphan)) = effects.orphan {
            // a booked orphan joins the books' unattributed line of its chain
            if let PoolStep::OrphanBooked { .. } = step {
                let mut books = self.book(&key.chain);
                books.unattributed = books
                    .unattributed
                    .checked_add(orphan.amount)
                    .unwrap_or(TokenAmount::MAX);
                self.store.put_book(key.chain, books);
            }
            self.store.put_pool_orphan(key, orphan);
        }
        if let Some(item) = effects.moved {
            self.store.put_pool_move(item.id, item);
        }
        // the canister's own transactions a CCTP finish names with no fee step: a move's or
        // a payout's, once it succeeded at depth
        match step {
            PoolStep::MoveBurned { id, tx_hash, .. } => self
                .store
                .put_pool_own_source(*tx_hash, types::pool::OwnSource::Move(*id)),
            PoolStep::PayoutPaid { id, tx_hash, .. } => self
                .store
                .put_pool_own_source(*tx_hash, types::pool::OwnSource::Payout(*id)),
            // one rescue per order, answered by the line that signed it
            PoolStep::EcoRescueSigned { order_id, .. } => self
                .store
                .put_eco_rescue(PoolRef::new(*order_id), index.get()),
            _ => {}
        }
    }

    /// A line that records a signed transaction's bytes, indexed for `signed_txs`.
    fn record_signed(&mut self, index: EventIndex) {
        self.store.put_signed(index);
    }

    fn record_event(&mut self, index: EventIndex) {
        let meta = self.store.meta();
        self.store.put_meta(LedgerMeta {
            next_event_index: index
                .next()
                .expect("BUG: append_event refuses an index without a successor"),
            ..meta
        });
    }

    /// The allocation rule: the nonce the event carries was the next one, so the next one
    /// is now the one after it. A nonce past `u64::MAX` is refused by the guard. The number
    /// handed out is held as unsigned until a signed record or a cancel spends it, so rule
    /// A5 can be read off the fold rather than inferred from what is missing.
    fn record_nonce_allocated(
        &mut self,
        chain_id: ChainId,
        nonce: Nonce,
        purpose: TxPurpose,
        at: Timestamp,
        action: Option<types::config::GasAction>,
    ) {
        self.store.put_next_nonce(
            chain_id,
            nonce
                .next()
                .expect("BUG: State::check refuses a nonce without a successor"),
        );
        self.store.put_unsigned_nonce(
            NonceKey { chain_id, nonce },
            UnsignedTx {
                purpose,
                created_at: at,
                action,
            },
        );
    }

    /// The other ways an allocation ends: the nonce was spent by a cancel rather than by the
    /// transaction it was made for, or by a pull's own record, which names its number
    /// because no swap holds it. Either way it stops being unsigned and nothing else moves.
    fn record_nonce_spent(&mut self, chain_id: ChainId, nonce: Nonce) {
        self.store
            .remove_unsigned_nonce(&NonceKey { chain_id, nonce });
    }

    /// A flow step the guard admitted.
    /// `action` is the gas action the step's signed transaction was created under, for a
    /// step that records one.
    fn record_flow(
        &mut self,
        quote_hash: QuoteHash,
        step: &types::FlowStep,
        index: EventIndex,
        at: Timestamp,
        action: Option<types::config::GasAction>,
    ) {
        let mut flow = types::Flow::apply(self.store.flow(&quote_hash), step, index.get(), at);
        if action.is_some() {
            flow.gas_action = action;
        }
        self.store.put_flow(quote_hash, flow);
    }

    /// The CCTP finish one of whose transactions is `tx`: its first or a replacement, indexed
    /// as each is signed. Only one of them lands, so a `tx` with a receipt is the one that
    /// did, closed or not.
    pub fn finish_landed_in(&self, tx: &TxHash) -> Option<Flow> {
        let quote_hash = self.store.flow_by_tx(tx)?;
        self.store
            .flow(&quote_hash)
            .filter(|flow| flow.kind == types::FlowKind::Finish)
    }

    /// A flow's replacement transaction, indexed with the flow's first.
    fn record_flow_replaced(&mut self, purpose: TxPurpose, tx: TxHash) {
        if let TxPurpose::Relay(quote_hash) | TxPurpose::Finish(quote_hash) = purpose {
            self.store.put_flow_tx(tx, quote_hash);
        }
    }

    /// The finish `tx` was the source transaction of, if one was.
    pub fn finish_of_source(&self, tx: &TxHash) -> Option<(QuoteHash, Flow)> {
        let quote_hash = self.store.flow_by_source(tx)?;
        self.store
            .flow(&quote_hash)
            .filter(|flow| flow.source_tx.as_ref() == Some(tx))
            .map(|flow| (quote_hash, flow))
    }

    /// How many arrivals other than `arrival` were finished from source transaction `tx`. The
    /// index answers the common case, a source with no finish yet, with no scan.
    pub fn other_arrivals_of_source(&self, tx: &TxHash, arrival: [u8; 32]) -> u32 {
        if self.finish_of_source(tx).is_none() {
            return 0;
        }
        let others: std::collections::BTreeSet<[u8; 32]> = self
            .store
            .flows()
            .into_iter()
            .filter(|(_, flow)| flow.source_tx.as_ref() == Some(tx))
            .filter_map(|(_, flow)| flow.arrival)
            .filter(|other| *other != arrival)
            .collect();
        u32::try_from(others.len()).unwrap_or(u32::MAX)
    }

    /// The flow under `quote_hash`, if any.
    pub fn flow(&self, quote_hash: &QuoteHash) -> Option<Flow> {
        self.store.flow(quote_hash)
    }
}
