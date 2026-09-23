use minicbor::{Decode, Encode};
use std::collections::{BTreeMap, BTreeSet};
use types::events::TxPurpose;
use types::{
    Attempt, ChainId, Choice, EventHash, EventIndex, LedgerMeta, Leg, Nonce, NonceKey, Outcome,
    Pocket, QuoteHash, Swap, SwapStatus, Timestamp, TokenAmount, TransitionError, TxHash,
    UnsignedTx, WaitingKey,
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
    /// The number the next transaction on `chain_id` must carry. A chain nothing was ever
    /// sent on is at zero: the allocator counts the transactions this canister created and
    /// never reads a chain to decide (rule A4).
    fn next_nonce(&self, chain_id: &ChainId) -> Nonce;
    fn put_next_nonce(&mut self, chain_id: ChainId, nonce: Nonce);
    /// Every chain the allocator has handed out a nonce on, in chain id order.
    fn nonces(&self) -> Vec<(ChainId, Nonce)>;
    /// The nonce `TxCreated` handed out at `key` and no `TxSigned` or `TxCancelled` has
    /// ended, if there is one.
    fn unsigned_nonce(&self, key: &NonceKey) -> Option<UnsignedTx>;
    fn put_unsigned_nonce(&mut self, key: NonceKey, unsigned: UnsignedTx);
    fn remove_unsigned_nonce(&mut self, key: &NonceKey);
    /// Every nonce still waiting for its signed record, in chain then nonce order. The
    /// collection holds one entry per send between its `TxCreated` and its `TxSigned`, plus
    /// the allocations whose transaction never came back, and the outbox pass ends those
    /// within a batch window: it is small by construction, so a walk of it is the read.
    fn unsigned_nonces(&self) -> Vec<(NonceKey, UnsignedTx)>;

    /// Whether `quote_hash` already holds a nonce it has not signed for. Rule A4 read from
    /// the swap's side: one swap never has two numbers out at once, so two sends that
    /// interleave at the signature cannot both allocate.
    fn has_unsigned_nonce_for(&self, quote_hash: &QuoteHash) -> bool {
        self.unsigned_nonces()
            .iter()
            .any(|(_, unsigned)| unsigned.purpose.quote_hash().as_ref() == Some(quote_hash))
    }

    /// The nonce `quote_hash` holds unsigned, if it holds one. `TxSigned` names a swap and
    /// not a nonce, so this is how the signed record finds the allocation it spends.
    fn unsigned_nonce_of(&self, quote_hash: &QuoteHash) -> Option<NonceKey> {
        self.unsigned_nonces()
            .into_iter()
            .find(|(_, unsigned)| unsigned.purpose.quote_hash().as_ref() == Some(quote_hash))
            .map(|(key, _)| key)
    }
    /// Indexes a waiting swap whose quote asks for an automatic refund. A swap that waits
    /// for a human is not indexed: the index is the queue the expiry timer works through,
    /// and nothing in it is work the timer can do.
    fn put_auto_refund_waiting(&mut self, key: WaitingKey);
    fn remove_auto_refund_waiting(&mut self, key: &WaitingKey);
    /// The first `limit` indexed waits, longest wait first, stopping early at the first one
    /// that began at `before` or later, and among them only the ones naming `naming` when
    /// one is given. The one walk of the index, and the readers below are written over it.
    /// Answers owned keys, and only the keys asked for, so no caller ever runs with the
    /// index borrowed or copies more of it than it reads.
    fn waiting_keys(
        &self,
        before: Option<Timestamp>,
        naming: Option<QuoteHash>,
        limit: usize,
    ) -> Vec<WaitingKey>;

    /// Every indexed wait, longest wait first.
    fn auto_refund_waiting(&self) -> Vec<WaitingKey> {
        self.waiting_keys(None, None, usize::MAX)
    }

    /// The indexed waits that began before `cutoff`, longest first, at most `limit` of them:
    /// the cost is the keys returned and never the swaps ever recorded.
    fn auto_refund_waiting_since_before(&self, cutoff: Timestamp, limit: usize) -> Vec<WaitingKey> {
        self.waiting_keys(Some(cutoff), None, limit)
    }

    /// Makes the index hold, for `quote_hash`, exactly `implied`: drops every other entry
    /// naming it, and writes `implied` if it is missing. The index is keyed by wait first,
    /// so a swap's entries are found by a walk and not by a seek: only the repair of a
    /// divergence needs this, and a diverged entry may carry an instant its swap never had.
    fn repair_auto_refund_waiting(&mut self, quote_hash: &QuoteHash, implied: Option<WaitingKey>) {
        let doomed = self
            .waiting_keys(None, Some(*quote_hash), usize::MAX)
            .into_iter()
            .filter(|key| Some(*key) != implied);
        for key in doomed {
            self.remove_auto_refund_waiting(&key);
        }
        if let Some(key) = implied {
            self.put_auto_refund_waiting(key);
        }
    }
}

/// A [`Store`] on the heap: what unit tests and the replay audit fold into. The deep audit
/// saves one between its steps, so it is stored as minicbor: `#[n]` indices are
/// append-only, never renumbered or reused, and a new field is optional.
#[derive(Clone, Debug, Default, PartialEq, Eq, Encode, Decode)]
pub struct MemoryStore {
    #[n(0)]
    swaps: BTreeMap<QuoteHash, Swap>,
    #[n(1)]
    pockets: BTreeMap<ChainId, Pocket>,
    #[n(2)]
    meta: LedgerMeta,
    #[n(3)]
    auto_refund_waiting: BTreeSet<WaitingKey>,
    #[n(4)]
    nonces: BTreeMap<ChainId, Nonce>,
    #[n(5)]
    unsigned: BTreeMap<NonceKey, UnsignedTx>,
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

    fn put_auto_refund_waiting(&mut self, key: WaitingKey) {
        self.auto_refund_waiting.insert(key);
    }

    fn remove_auto_refund_waiting(&mut self, key: &WaitingKey) {
        self.auto_refund_waiting.remove(key);
    }

    fn waiting_keys(
        &self,
        before: Option<Timestamp>,
        naming: Option<QuoteHash>,
        limit: usize,
    ) -> Vec<WaitingKey> {
        self.auto_refund_waiting
            .iter()
            .take_while(|key| before.is_none_or(|cutoff| key.since < cutoff))
            .filter(|key| naming.is_none_or(|quote_hash| key.quote_hash == quote_hash))
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
            nonces,
            unsigned,
        } = &self.store;
        let other_pockets = other.store.pockets();
        let other_swaps = other.store.swaps();
        let other_nonces = other.store.nonces();
        let other_unsigned = other.store.unsigned_nonces();
        *meta == other.meta()
            && nonces
                .iter()
                .eq(other_nonces.iter().map(|(chain, nonce)| (chain, nonce)))
            && unsigned
                .iter()
                .eq(other_unsigned.iter().map(|(key, tx)| (key, tx)))
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
                last_leg: None,
                last_outcome: None,
                last_tx_hash: None,
                paid_out: None,
                fee_accrued: None,
            },
        );
    }

    /// The signed record spends the nonce the swap was holding. `TxSigned` names a swap and
    /// not a number, and it cannot start naming one without moving its canonical preimage,
    /// so the allocation is found by the swap: the `TxCreated` guard admits one unsigned
    /// nonce per swap, which is what makes that lookup single-valued, and the `TxSigned`
    /// guard admits no record for a swap holding none, which is what makes it total.
    fn record_attempt_signed(&mut self, quote_hash: &QuoteHash, attempt: Attempt) {
        let key = self
            .store
            .unsigned_nonce_of(quote_hash)
            .expect("BUG: State::check refuses a signed record for a swap holding no nonce");
        // the leg the attempt is, read off the allocation's purpose: `TxSigned` names no
        // leg, and the engine needs one to know where the swap is between its lines
        let leg = self
            .store
            .unsigned_nonce(&key)
            .and_then(|unsigned| Leg::of(unsigned.purpose));
        self.store.remove_unsigned_nonce(&key);
        self.update_swap(quote_hash, |swap| {
            swap.last_attempt = Some(attempt);
            swap.open_attempt = Some(attempt);
            swap.last_leg = leg;
            swap.last_outcome = None;
            swap.last_tx_hash = None;
            match swap.status {
                SwapStatus::FundsReceived => swap.status = SwapStatus::Executing,
                SwapStatus::PaidInStable => swap.status = SwapStatus::Delivering,
                _ => {}
            }
        });
    }

    /// The open attempt was confirmed or failed; either way it counts, and how it ended is
    /// kept for the engine, with the hash it confirmed as when it did.
    fn record_attempt_closed(
        &mut self,
        quote_hash: &QuoteHash,
        outcome: Outcome,
        tx_hash: Option<TxHash>,
    ) {
        self.update_swap(quote_hash, |swap| {
            swap.open_attempt = None;
            swap.last_outcome = Some(outcome);
            swap.last_tx_hash = tx_hash;
        });
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
        let indexed = self.update_swap(quote_hash, |swap| {
            swap.status = SwapStatus::WaitingForUser;
            swap.waiting_since = Some(at);
            // the FundsReceived guard binds the swap id to these bytes, so they parse.
            // Bytes an older wasm recorded that do not are a swap that waits for a human.
            swap.auto_refund_wait()
        });
        if let Some(since) = indexed {
            self.store.put_auto_refund_waiting(WaitingKey {
                since,
                quote_hash: *quote_hash,
            });
        }
    }

    /// Makes the index agree with the swap: drops every entry naming it that the swap does
    /// not imply, and writes the one it does. On the fold of a log the index already
    /// agrees, so the line changes nothing there; on an index no event could have produced
    /// it is the repair. Found by swap id rather than by key, because a diverged entry may
    /// carry an instant the swap never waited since.
    fn record_waiting_repaired(&mut self, quote_hash: &QuoteHash) {
        let implied = self
            .store
            .swap(quote_hash)
            .and_then(|swap| swap.auto_refund_wait())
            .map(|since| WaitingKey {
                since,
                quote_hash: *quote_hash,
            });
        self.store.repair_auto_refund_waiting(quote_hash, implied);
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
            },
        );
    }

    /// What the payout leg this canister just created will pay the user, read off the
    /// calldata the line carries: the record of a delivered swap is made from this, so it
    /// is folded from the log like everything else the record says.
    fn record_payout_created(&mut self, quote_hash: &QuoteHash, paid_out: Option<TokenAmount>) {
        self.update_swap(quote_hash, |swap| swap.paid_out = paid_out);
    }

    /// The other ways an allocation ends: the nonce was spent by a cancel rather than by the
    /// transaction it was made for, or by a pull's own record, which names its number
    /// because no swap holds it. Either way it stops being unsigned and nothing else moves.
    fn record_nonce_spent(&mut self, chain_id: ChainId, nonce: Nonce) {
        self.store
            .remove_unsigned_nonce(&NonceKey { chain_id, nonce });
    }

    fn record_fee_accrued(&mut self, quote_hash: &QuoteHash, amount: TokenAmount) {
        let meta = self.store.meta();
        self.store.put_meta(LedgerMeta {
            fees_accrued: meta
                .fees_accrued
                .checked_add(amount)
                .expect("BUG: State::check refuses a fee that overflows fees_accrued"),
            ..meta
        });
        // fees_accrued holds every swap's fees, this one's among them, so what fit there
        // fits here
        self.update_swap(quote_hash, |swap| {
            swap.fee_accrued = Some(swap.fee_accrued.map_or(amount, |accrued| {
                accrued
                    .checked_add(amount)
                    .expect("BUG: a swap's fees are part of fees_accrued, which did not overflow")
            }))
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
