use std::collections::{BTreeMap, BTreeSet};
use types::{
    Attempt, ChainId, Choice, EventHash, EventIndex, LedgerMeta, Pocket, Quote, QuoteHash, Swap,
    SwapStatus, Timestamp, TokenAmount, TransitionError, WaitingKey,
};

pub mod pending_quotes;
pub mod transitions;

/// Where the event log's fold lives. [`State`] reads and writes only through this, so one
/// copy of the transition rules serves every store.
pub trait Store {
    fn swap(&self, quote_hash: &QuoteHash) -> Option<Swap>;
    fn put_swap(&mut self, quote_hash: QuoteHash, swap: Swap);
    /// Every swap, in quote hash order.
    fn swaps(&self) -> Vec<(QuoteHash, Swap)>;
    fn pocket(&self, chain_id: &ChainId) -> Option<Pocket>;
    fn put_pocket(&mut self, chain_id: ChainId, pocket: Pocket);
    /// Every pocket, in chain id order.
    fn pockets(&self) -> Vec<(ChainId, Pocket)>;
    fn meta(&self) -> LedgerMeta;
    fn put_meta(&mut self, meta: LedgerMeta);
    /// Indexes a waiting swap whose quote asks for an automatic refund. A swap that waits
    /// for a human is not indexed: the index is the queue the expiry timer works through,
    /// and nothing in it is work the timer can do.
    fn put_auto_refund_waiting(&mut self, key: WaitingKey);
    fn remove_auto_refund_waiting(&mut self, key: &WaitingKey);
    /// Every indexed wait, longest wait first.
    fn auto_refund_waiting(&self) -> Vec<WaitingKey>;
    /// The indexed waits that began before `cutoff`, longest first, at most `limit` of them.
    /// Walks the index from its first key and stops at the first one that is not older, or at
    /// `limit`, so the cost is the keys returned and never the swaps ever recorded.
    fn auto_refund_waiting_since_before(&self, cutoff: Timestamp, limit: usize) -> Vec<WaitingKey>;
}

/// A [`Store`] on the heap: what unit tests and the replay audit fold into.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MemoryStore {
    swaps: BTreeMap<QuoteHash, Swap>,
    pockets: BTreeMap<ChainId, Pocket>,
    meta: LedgerMeta,
    auto_refund_waiting: BTreeSet<WaitingKey>,
}

impl Store for MemoryStore {
    fn swap(&self, quote_hash: &QuoteHash) -> Option<Swap> {
        self.swaps.get(quote_hash).cloned()
    }

    fn put_swap(&mut self, quote_hash: QuoteHash, swap: Swap) {
        self.swaps.insert(quote_hash, swap);
    }

    fn swaps(&self) -> Vec<(QuoteHash, Swap)> {
        self.swaps
            .iter()
            .map(|(hash, swap)| (*hash, swap.clone()))
            .collect()
    }

    fn pocket(&self, chain_id: &ChainId) -> Option<Pocket> {
        self.pockets.get(chain_id).copied()
    }

    fn put_pocket(&mut self, chain_id: ChainId, pocket: Pocket) {
        self.pockets.insert(chain_id, pocket);
    }

    fn pockets(&self) -> Vec<(ChainId, Pocket)> {
        self.pockets
            .iter()
            .map(|(chain, pocket)| (*chain, *pocket))
            .collect()
    }

    fn meta(&self) -> LedgerMeta {
        self.meta
    }

    fn put_meta(&mut self, meta: LedgerMeta) {
        self.meta = meta;
    }

    fn put_auto_refund_waiting(&mut self, key: WaitingKey) {
        self.auto_refund_waiting.insert(key);
    }

    fn remove_auto_refund_waiting(&mut self, key: &WaitingKey) {
        self.auto_refund_waiting.remove(key);
    }

    fn auto_refund_waiting(&self) -> Vec<WaitingKey> {
        self.auto_refund_waiting.iter().copied().collect()
    }

    fn auto_refund_waiting_since_before(&self, cutoff: Timestamp, limit: usize) -> Vec<WaitingKey> {
        self.auto_refund_waiting
            .iter()
            .take_while(|key| key.since < cutoff)
            .take(limit)
            .copied()
            .collect()
    }
}

/// The fold of the event log. [`State::check`] decides whether an event may happen, and
/// [`transitions::apply_state_transition`] records one that did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct State<S> {
    store: S,
}

impl State<MemoryStore> {
    /// Whether `other` holds exactly this heap fold, whatever its store. The destructure has
    /// no `..`, so a collection added to the store fails to compile until it is compared.
    pub fn matches<T: Store>(&self, other: &State<T>) -> bool {
        let MemoryStore {
            swaps,
            pockets,
            meta,
            auto_refund_waiting,
        } = &self.store;
        let other_pockets = other.store.pockets();
        let other_swaps = other.store.swaps();
        *meta == other.meta()
            && auto_refund_waiting
                .iter()
                .eq(other.store.auto_refund_waiting().iter())
            && pockets
                .iter()
                .eq(other_pockets.iter().map(|(chain, pocket)| (chain, pocket)))
            && swaps
                .iter()
                .eq(other_swaps.iter().map(|(hash, swap)| (hash, swap)))
    }
}

impl<S: Store> State<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }

    pub fn store(&self) -> &S {
        &self.store
    }

    pub fn meta(&self) -> LedgerMeta {
        self.store.meta()
    }

    pub fn swap(&self, quote_hash: &QuoteHash) -> Result<Swap, TransitionError> {
        self.store
            .swap(quote_hash)
            .ok_or(TransitionError::UnknownSwap(*quote_hash))
    }

    pub fn pocket(&self, chain_id: &ChainId) -> Result<Pocket, TransitionError> {
        self.store
            .pocket(chain_id)
            .ok_or(TransitionError::UnknownPocket(*chain_id))
    }

    /// The chain's pocket, or an empty one where none was funded yet.
    fn pocket_or_empty(&self, chain_id: &ChainId) -> Pocket {
        self.store.pocket(chain_id).unwrap_or_default()
    }

    fn update_swap<R>(&mut self, quote_hash: &QuoteHash, update: impl FnOnce(&mut Swap) -> R) -> R {
        let mut swap = self
            .store
            .swap(quote_hash)
            .expect("BUG: State::check refuses every event on an unknown swap but FundsReceived");
        let result = update(&mut swap);
        self.store.put_swap(*quote_hash, swap);
        result
    }

    /// Drops a swap from the waiting index once its clock has stopped. A swap that was not
    /// waiting had no clock and no entry, and a swap that waited for a human had a clock and
    /// no entry: removing a key the index does not hold is nothing.
    fn stop_waiting(&mut self, quote_hash: &QuoteHash, stopped: Option<Timestamp>) {
        if let Some(since) = stopped {
            self.store.remove_auto_refund_waiting(&WaitingKey {
                since,
                quote_hash: *quote_hash,
            });
        }
    }

    fn record_event(&mut self, index: EventIndex, hash: EventHash) {
        let meta = self.store.meta();
        self.store.put_meta(LedgerMeta {
            next_event_index: index
                .next()
                .expect("BUG: append_event refuses an index without a successor"),
            last_event_hash: hash,
            ..meta
        });
    }

    fn record_funds_received(
        &mut self,
        quote_hash: QuoteHash,
        quote_bytes: Vec<u8>,
        src_chain: ChainId,
        src_token: types::TokenId,
        amount_in: TokenAmount,
    ) {
        self.store.put_swap(
            quote_hash,
            Swap {
                quote_bytes,
                status: SwapStatus::FundsReceived,
                last_attempt: None,
                open_attempt: None,
                src_chain,
                src_token,
                amount_in,
                amount_paid: None,
                waiting_since: None,
            },
        );
    }

    fn record_attempt_signed(&mut self, quote_hash: &QuoteHash, attempt: Attempt) {
        self.update_swap(quote_hash, |swap| {
            swap.last_attempt = Some(attempt);
            swap.open_attempt = Some(attempt);
            match swap.status {
                SwapStatus::FundsReceived => swap.status = SwapStatus::Executing,
                SwapStatus::PaidInStable => swap.status = SwapStatus::Delivering,
                _ => {}
            }
        });
    }

    /// The open attempt was confirmed or failed; either way it counts.
    fn record_attempt_closed(&mut self, quote_hash: &QuoteHash) {
        self.update_swap(quote_hash, |swap| swap.open_attempt = None);
    }

    fn record_paid_in_stable(&mut self, quote_hash: &QuoteHash, amount: TokenAmount) {
        self.update_swap(quote_hash, |swap| {
            swap.status = SwapStatus::PaidInStable;
            swap.amount_paid = Some(amount);
        });
    }

    /// The one step that starts a waiting clock, so the one step that indexes a swap. Only a
    /// swap whose quote asks for an automatic refund goes into the index: the index is the
    /// queue the expiry timer works through, and a swap that waits for a human is not work
    /// the timer can do. Its wait is on the swap itself either way, so `get_swap` shows it.
    fn record_decision_required(&mut self, quote_hash: &QuoteHash, at: Timestamp) {
        let auto_refund = self.update_swap(quote_hash, |swap| {
            swap.status = SwapStatus::WaitingForUser;
            swap.waiting_since = Some(at);
            // the FundsReceived guard binds the swap id to these bytes, so they parse.
            // Bytes an older wasm recorded that do not are a swap that waits for a human.
            Quote::parse(&swap.quote_bytes).is_ok_and(|quote| quote.auto_refund)
        });
        if auto_refund {
            self.store.put_auto_refund_waiting(WaitingKey {
                since: at,
                quote_hash: *quote_hash,
            });
        }
    }

    fn record_decision_made(&mut self, quote_hash: &QuoteHash, choice: Choice) {
        let stopped = self.update_swap(quote_hash, |swap| {
            swap.status = match choice {
                Choice::Requote => SwapStatus::Executing,
                Choice::Refund => SwapStatus::Refunding,
            };
            swap.waiting_since.take()
        });
        self.stop_waiting(quote_hash, stopped);
    }

    /// A refund answers any open question, so the waiting clock stops with it.
    fn record_refund_started(&mut self, quote_hash: &QuoteHash) {
        let stopped = self.update_swap(quote_hash, |swap| {
            swap.status = SwapStatus::Refunding;
            swap.waiting_since.take()
        });
        self.stop_waiting(quote_hash, stopped);
    }

    fn record_refunded(&mut self, quote_hash: &QuoteHash) {
        self.update_swap(quote_hash, |swap| swap.status = SwapStatus::Refunded);
    }

    fn record_swap_done(&mut self, quote_hash: &QuoteHash) {
        self.update_swap(quote_hash, |swap| swap.status = SwapStatus::Done);
    }

    fn record_frozen(&mut self, quote_hash: &QuoteHash) {
        let stopped = self.update_swap(quote_hash, |swap| {
            swap.status = SwapStatus::Frozen;
            swap.waiting_since.take()
        });
        self.stop_waiting(quote_hash, stopped);
    }

    fn record_fee_accrued(&mut self, amount: TokenAmount) {
        let meta = self.store.meta();
        self.store.put_meta(LedgerMeta {
            fees_accrued: meta
                .fees_accrued
                .checked_add(amount)
                .expect("BUG: State::check refuses a fee that overflows fees_accrued"),
            ..meta
        });
    }

    fn record_pocket_funded(&mut self, chain_id: ChainId, amount: TokenAmount) {
        let pocket = self
            .pocket_or_empty(&chain_id)
            .fund(amount)
            .expect("BUG: State::check ran Pocket::fund on this pocket");
        self.store.put_pocket(chain_id, pocket);
    }

    fn record_pocket_reserved(&mut self, chain_id: ChainId, amount: TokenAmount) {
        let pocket = self
            .pocket_or_empty(&chain_id)
            .reserve(amount)
            .expect("BUG: State::check ran Pocket::reserve on this pocket");
        self.store.put_pocket(chain_id, pocket);
    }

    fn record_pocket_released(&mut self, chain_id: ChainId, amount: TokenAmount) {
        let pocket = self
            .pocket_or_empty(&chain_id)
            .release(amount)
            .expect("BUG: State::check ran Pocket::release on this pocket");
        self.store.put_pocket(chain_id, pocket);
    }

    /// The settle debit: the value left the pocket on-chain, so it does not come back.
    fn record_pocket_spent(&mut self, chain_id: ChainId, amount: TokenAmount) {
        let pocket = self
            .pocket_or_empty(&chain_id)
            .spend(amount)
            .expect("BUG: State::check ran Pocket::spend on this pocket");
        self.store.put_pocket(chain_id, pocket);
    }

    /// Withdraws before it funds, so a rebalance onto the same chain nets to nothing.
    fn record_pocket_rebalanced(&mut self, from: ChainId, to: ChainId, amount: TokenAmount) {
        let source = self
            .pocket_or_empty(&from)
            .withdraw(amount)
            .expect("BUG: State::check ran Pocket::withdraw on the source pocket");
        self.store.put_pocket(from, source);
        let destination = self.pocket_or_empty(&to).fund(amount).expect(
            "BUG: State::check ran Pocket::fund on the destination pocket after the withdraw",
        );
        self.store.put_pocket(to, destination);
    }

    /// Test-only: flips one bit of the chain head, and nothing else.
    #[cfg(feature = "inttest")]
    pub fn skew_chain_head(&mut self) {
        let meta = self.store.meta();
        let mut head = meta.last_event_hash.into_bytes();
        head[0] ^= 1;
        self.store.put_meta(LedgerMeta {
            last_event_hash: EventHash::new(head),
            ..meta
        });
    }
}
