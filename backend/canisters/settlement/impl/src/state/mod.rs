use minicbor::{Decode, Encode};
use std::collections::BTreeMap;
use types::{
    Attempt, ChainId, Choice, EventHash, EventIndex, Pocket, QuoteHash, Swap, SwapStatus,
    Timestamp, TokenAmount, TransitionError,
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
}

/// What the fold keeps besides swaps and pockets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Encode, Decode)]
pub struct LedgerMeta {
    #[n(0)]
    pub fees_accrued: TokenAmount,
    /// The index the next event is sealed at.
    #[n(1)]
    pub next_event_index: EventIndex,
    /// The chain head the next event links to.
    #[n(2)]
    pub last_event_hash: EventHash,
}

types::storable_as_cbor!(LedgerMeta);

/// A [`Store`] on the heap: what unit tests and the replay audit fold into.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MemoryStore {
    swaps: BTreeMap<QuoteHash, Swap>,
    pockets: BTreeMap<ChainId, Pocket>,
    meta: LedgerMeta,
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
        } = &self.store;
        let other_pockets = other.store.pockets();
        let other_swaps = other.store.swaps();
        *meta == other.meta()
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

    fn update_swap(&mut self, quote_hash: &QuoteHash, update: impl FnOnce(&mut Swap)) {
        let mut swap = self
            .store
            .swap(quote_hash)
            .expect("BUG: State::check refuses every event on an unknown swap but FundsReceived");
        update(&mut swap);
        self.store.put_swap(*quote_hash, swap);
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

    fn record_decision_required(&mut self, quote_hash: &QuoteHash, at: Timestamp) {
        self.update_swap(quote_hash, |swap| {
            swap.status = SwapStatus::WaitingForUser;
            swap.waiting_since = Some(at);
        });
    }

    fn record_decision_made(&mut self, quote_hash: &QuoteHash, choice: Choice) {
        self.update_swap(quote_hash, |swap| {
            swap.waiting_since = None;
            swap.status = match choice {
                Choice::Requote => SwapStatus::Executing,
                Choice::Refund => SwapStatus::Refunding,
            };
        });
    }

    /// A refund answers any open question, so the waiting clock stops with it.
    fn record_refund_started(&mut self, quote_hash: &QuoteHash) {
        self.update_swap(quote_hash, |swap| {
            swap.status = SwapStatus::Refunding;
            swap.waiting_since = None;
        });
    }

    fn record_refunded(&mut self, quote_hash: &QuoteHash) {
        self.update_swap(quote_hash, |swap| swap.status = SwapStatus::Refunded);
    }

    fn record_swap_done(&mut self, quote_hash: &QuoteHash) {
        self.update_swap(quote_hash, |swap| swap.status = SwapStatus::Done);
    }

    fn record_frozen(&mut self, quote_hash: &QuoteHash) {
        self.update_swap(quote_hash, |swap| {
            swap.status = SwapStatus::Frozen;
            swap.waiting_since = None;
        });
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
