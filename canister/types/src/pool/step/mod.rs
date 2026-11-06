//! The pool's steps: every line the pool's part of the log records, and what each one says
//! about itself (its name, its bytes, the transaction it signs, whether it moves a payout).

use super::*;

/// One step of the pool, as the log records it.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub enum PoolStep {
    /// A controller set the pool's knobs, with the ledger fees read for them.
    #[n(0)]
    SettingsSet {
        #[n(0)]
        settings: PoolSettings,
    },
    /// The quoter registered a deposit at the user's commit.
    #[n(1)]
    Registered {
        #[n(0)]
        r#ref: PoolRef,
        #[n(1)]
        deposit: Registration,
    },
    /// A registration ran out with nothing arrived: its hold is released.
    #[n(2)]
    Expired {
        #[n(0)]
        r#ref: PoolRef,
    },
    /// The canister read a transfer to its Base address proving this deposit.
    #[n(3)]
    Arrived {
        #[n(0)]
        r#ref: PoolRef,
        #[n(1)]
        credit: CreditKey,
        #[n(2)]
        amount: TokenAmount,
        #[n(3)]
        block: BlockNumber,
    },
    /// A transfer to the canister that proves no deposit it can credit: kept for the
    /// orphan refund with its payer when known.
    #[n(4)]
    Unattributed {
        #[n(0)]
        credit: CreditKey,
        #[n(1)]
        amount: TokenAmount,
        #[n(2)]
        src_payer: Option<EvmAddress>,
        #[n(3)]
        why: String,
        #[n(4)]
        block: BlockNumber,
        /// The registration it named, if any: a late or short arrival closes it.
        #[n(5)]
        r#ref: Option<PoolRef>,
        /// A deposit's transfer the canister's own read found without its terms:
        /// it awaits them until then under `r#ref`, then is held for a controller.
        #[n(6)]
        awaiting_until: Option<Timestamp>,
    },
    /// A swap's payout began: its dedup pair and path, before any call.
    #[n(5)]
    PayoutStarted {
        #[n(0)]
        r#ref: PoolRef,
        #[n(1)]
        attempt: PayoutAttempt,
    },
    /// A swap's recipient was paid.
    #[n(6)]
    Paid {
        #[n(0)]
        r#ref: PoolRef,
        #[cbor(n(1), with = "principal_cbor")]
        token: Principal,
        #[n(2)]
        amount: TokenAmount,
        #[n(3)]
        block: u64,
        /// The ckUSDC that left the ICP pocket, every ledger fee included.
        #[n(4)]
        spent: TokenAmount,
    },
    /// The ICPSwap leg could not reach the minimum: all of it is paid as ckUSDC instead.
    /// `r#ref` names a deposit or a withdrawal.
    #[n(7)]
    FellBack {
        #[n(0)]
        r#ref: PoolRef,
        #[n(1)]
        why: String,
        /// The ckUSDC transfer that pays all of it, fixed now for every retry.
        #[n(2)]
        amount: Option<TokenAmount>,
        #[n(3)]
        token: Option<PrincipalBytes>,
    },
    /// A stake's shares were minted.
    #[n(8)]
    Minted {
        #[n(0)]
        r#ref: PoolRef,
        #[n(1)]
        owner: PoolAccount,
        #[n(2)]
        shares: Shares,
    },
    /// An ICP stake's pull landed and its shares were minted.
    #[n(9)]
    StakedIcp {
        #[n(0)]
        id: PoolOpId,
        #[n(1)]
        owner: PoolAccount,
        #[n(2)]
        amount: TokenAmount,
        #[n(3)]
        move_fee: TokenAmount,
        #[n(4)]
        shares: Shares,
        #[n(5)]
        block: u64,
    },
    /// An ICP withdrawal was paid.
    #[n(10)]
    WithdrawnIcp {
        #[n(0)]
        id: PoolOpId,
        #[n(1)]
        owner: IcpAccount,
        #[n(2)]
        burned: Shares,
        #[n(3)]
        usdc: TokenAmount,
        #[cbor(n(4), with = "principal_cbor")]
        token: Principal,
        #[n(5)]
        paid: TokenAmount,
        #[n(6)]
        block: u64,
        #[n(7)]
        spent: TokenAmount,
    },
    /// A payout's drive failed and will be tried again, until [`MAX_PAYOUT_ATTEMPTS`].
    /// `r#ref` names a deposit or a withdrawal.
    #[n(11)]
    AttemptFailed {
        #[n(0)]
        r#ref: PoolRef,
        #[n(1)]
        why: String,
    },
    /// The pool's quote was below the minimum: tried once more on the next drive.
    #[n(12)]
    SwapDeclined {
        #[n(0)]
        r#ref: PoolRef,
        #[n(1)]
        why: String,
        /// The leg it names; none for the first.
        #[n(2)]
        leg: Option<u8>,
    },
    /// `depositFromAndSwap` is about to be called.
    #[n(13)]
    SwapBegun {
        #[n(0)]
        r#ref: PoolRef,
        /// What the pool held of the canister's just before, read while this leg alone was
        /// at the pool: only what is there above it is this leg's. None on a leg from before
        /// it was read.
        #[n(1)]
        before: Option<PoolHeld>,
        /// The leg it names; none for the first.
        #[n(2)]
        leg: Option<u8>,
        /// What the leg deposits, its pull's fee aside. None on a leg from before it was
        /// recorded, whose payout's attempt holds it.
        #[n(3)]
        amount_in: Option<TokenAmount>,
    },
    /// `depositFromAndSwap` answered its output.
    #[n(14)]
    Swapped {
        #[n(0)]
        r#ref: PoolRef,
        #[n(1)]
        out: TokenAmount,
        /// The output's transfer to the recipient, fixed now for every retry.
        #[n(2)]
        send: Option<TokenAmount>,
        /// The leg it names; none for the first.
        #[n(3)]
        leg: Option<u8>,
    },
    /// An ICP stake's pull is about to be asked: its TVL room is held and its dedup pair set.
    #[n(15)]
    StakeStarted {
        #[n(0)]
        id: PoolOpId,
        #[n(1)]
        owner: PoolAccount,
        #[n(2)]
        from: IcpAccount,
        #[n(3)]
        amount: TokenAmount,
        #[n(4)]
        created_at: Timestamp,
        /// Swapic's move fee out of `amount`, the cost of one ICP to Base move.
        #[n(5)]
        move_fee: TokenAmount,
    },
    /// The ledger refused an ICP stake's pull: nothing moved, the room is released.
    #[n(16)]
    StakeFailed {
        #[n(0)]
        id: PoolOpId,
        #[n(1)]
        why: String,
    },
    /// An ICP withdrawal burned its shares and reserved its ckUSDC, before any call.
    #[n(17)]
    WithdrawStarted {
        #[n(0)]
        id: PoolOpId,
        #[n(1)]
        owner: IcpAccount,
        #[n(2)]
        burned: Shares,
        #[n(3)]
        usdc: TokenAmount,
        #[n(4)]
        to: IcpAccount,
        #[cbor(n(5), with = "principal_cbor")]
        token_out: Principal,
        #[n(6)]
        min_out: u128,
        #[n(7)]
        attempt: PayoutAttempt,
        /// Swapic's move fee out of `usdc`, the cost of one Base to ICP move.
        #[n(8)]
        move_fee: TokenAmount,
    },
    /// The ICP pocket's balance check, when it found a shortfall or a surplus other than
    /// the last one recorded.
    #[n(18)]
    IcpChecked {
        #[n(0)]
        check: IcpCheck,
    },
    /// The ICPSwap leg's pool was approved for its amount in: one more ckUSDC fee spent.
    #[n(21)]
    SwapApproved {
        #[n(0)]
        r#ref: PoolRef,
        /// The leg it names; none for the first.
        #[n(1)]
        leg: Option<u8>,
    },
    /// A swap that did not happen was undone: the ckUSDC it deposited, if any, withdrawn back
    /// from the pool. `cost` is what its deposit took from the canister's ckUSDC account in
    /// the end (zero when the deposit never happened). All of it is paid as ckUSDC next.
    #[n(22)]
    SwapUndone {
        #[n(0)]
        r#ref: PoolRef,
        #[n(1)]
        cost: TokenAmount,
        /// The ckUSDC withdrawn from the pool: this leg's own, never more than it deposited.
        #[n(2)]
        withdrawn: TokenAmount,
        /// The leg it names; none for the first.
        #[n(3)]
        leg: Option<u8>,
    },
    /// A controller set the Base payouts' knobs.
    #[n(23)]
    PayoutSettingsSet {
        #[n(0)]
        settings: PayoutSettings,
    },
    /// A payout from the Base pocket was opened with what undoes it: an ICP swap before its
    /// pull, a withdrawal with its shares burned and its nonce spent.
    #[n(24)]
    PayoutOpened {
        #[n(0)]
        id: u64,
        /// Boxed: a log line's size is its largest step's.
        #[n(1)]
        kind: Box<PayoutKind>,
        #[n(2)]
        terms: PayoutTerms,
        #[n(3)]
        undo: Undo,
        /// A planned ICP swap's payout plan as the user handed it in, boxed.
        #[n(4)]
        plan: Option<Box<PinnedOrder>>,
        /// A swap paid through the ck minter ([`CkLeg`]), never from the Base pocket.
        #[n(5)]
        ck: Flag,
    },
    /// The payout's exact amount is known: what its order spends on Base. An ICP swap's
    /// ckUSDC (`received`) is held on ICP until it is paid, its network fee on the ICP fee
    /// line; a withdrawal's network fee is held on Base with it.
    #[n(25)]
    PayoutAmountKnown {
        #[n(0)]
        id: u64,
        #[n(1)]
        amount: TokenAmount,
        #[n(2)]
        received: Option<TokenAmount>,
    },
    /// The Base pocket cannot cover it behind the queue: it waits, first in first out.
    #[n(26)]
    PayoutQueued {
        #[n(0)]
        id: u64,
    },
    /// Its Base hold is reserved: it waits for an order.
    #[n(27)]
    PayoutWaiting {
        #[n(0)]
        id: u64,
    },
    /// The canister signed `run(order)` for it: the transaction's exact bytes, recorded
    /// before they are broadcast, spending the nonce `TxCreated` handed out for it.
    #[n(28)]
    PayoutSigned {
        #[n(0)]
        id: u64,
        #[n(1)]
        order: PinnedOrder,
        #[n(2)]
        chain_id: ChainId,
        #[n(3)]
        nonce: Nonce,
        #[n(4)]
        tx_hash: TxHash,
        #[cbor(n(5), with = "minicbor::bytes")]
        raw_tx: Vec<u8>,
    },
    /// Its transaction succeeded at depth: the USDC left the Base pocket.
    #[n(29)]
    PayoutPaid {
        #[n(0)]
        id: u64,
        #[n(1)]
        tx_hash: TxHash,
        #[n(2)]
        block: BlockNumber,
        /// The Eco intent the order published, read off the receipt.
        #[cbor(n(3), with = "minicbor::bytes")]
        intent: Option<[u8; 32]>,
    },
    /// Its transaction reverted at depth: its USDC reserved again for its undo, which comes
    /// once Swapic says its order never ran; never signed again.
    #[n(30)]
    PayoutReverted {
        #[n(0)]
        id: u64,
        /// The block its transaction reverted in: `Swapic.used` is read at it or after.
        #[n(1)]
        block: Option<BlockNumber>,
    },
    /// Its undo began: an ICP refund is about to be asked, nothing is signed for it again.
    #[n(31)]
    PayoutUndoing {
        #[n(0)]
        id: u64,
        #[n(1)]
        why: String,
    },
    /// It was undone: an ICP swap refunded (`block`: the refund's ledger block), a
    /// withdrawal's shares minted back.
    #[n(32)]
    PayoutUndone {
        #[n(0)]
        id: u64,
        #[n(1)]
        why: String,
        #[n(2)]
        block: Option<u64>,
    },
    /// A controller set the ICP routes, with the ICP ledger and every route ledger's fee
    /// read for them.
    #[n(33)]
    IcpRoutesSet {
        #[n(0)]
        routes: IcpRoutes,
    },
    /// The canister signed `USDC.approve(Swapic, max)` on Base: the transaction's exact bytes,
    /// recorded before they are broadcast, spending the nonce `TxCreated` handed out for it.
    #[n(34)]
    ApproveSigned {
        #[n(0)]
        chain_id: ChainId,
        #[n(1)]
        nonce: Nonce,
        #[n(2)]
        tx_hash: TxHash,
        #[cbor(n(3), with = "minicbor::bytes")]
        raw_tx: Vec<u8>,
    },
    /// An ICP swap's pull landed (`block`, its ledger block): its legs to ckUSDC run next,
    /// on the path and with the dedup pair `attempt` fixes (none for ckUSDC, which has none).
    #[n(35)]
    PayoutPulled {
        #[n(0)]
        id: u64,
        #[n(1)]
        block: u64,
        #[n(2)]
        attempt: Option<PayoutAttempt>,
    },
    /// A payout stopped for a human: what happened to its money is not known.
    #[n(36)]
    PayoutStopped {
        #[n(0)]
        id: u64,
        #[n(1)]
        why: String,
    },
    /// A waiting payout waited `order_wait` with no valid order: listed again, at the back.
    #[n(37)]
    PayoutRelisted {
        #[n(0)]
        id: u64,
    },
    /// The Eco intent a paid payout's order published went unfilled and Eco refunded its
    /// reward to its creator, the canister: the USDC is back at the canister's Base address
    /// (credited once, under its transfer's key), and the payout waits again for a new order,
    /// its first intent never to be filled.
    #[n(38)]
    PayoutRefunded {
        #[n(0)]
        id: u64,
        #[n(1)]
        credit: CreditKey,
        #[n(2)]
        amount: TokenAmount,
        #[n(3)]
        block: BlockNumber,
    },
    /// A controller booked an orphan to the Base books' unattributed line.
    #[n(39)]
    OrphanBooked {
        #[n(0)]
        key: CreditKey,
    },
    /// The canister signed an orphan's refund, `USDC.transfer(to, sent)` on Base: the
    /// transaction's exact bytes, spending the nonce `TxCreated` handed out for it. What it
    /// keeps of the orphan (the least network fee) goes to the Base fee line.
    #[n(40)]
    OrphanRefundSigned {
        #[n(0)]
        key: CreditKey,
        #[n(1)]
        to: EvmAddress,
        #[n(2)]
        sent: TokenAmount,
        #[n(3)]
        chain_id: ChainId,
        #[n(4)]
        nonce: Nonce,
        #[n(5)]
        tx_hash: TxHash,
        #[cbor(n(6), with = "minicbor::bytes")]
        raw_tx: Vec<u8>,
    },
    /// An orphan's refund closed at depth: refunded, or reverted and held again.
    #[n(41)]
    OrphanRefundClosed {
        #[n(0)]
        key: CreditKey,
        #[n(1)]
        success: bool,
    },
    /// The terms of a transfer that awaited them came: out of the unattributed line,
    /// for the deposit's own arrival line, which follows in the same message; or, `refused`,
    /// the terms the pool does not take, and the transfer is held for a controller with why.
    #[n(42)]
    TermsClaimed {
        #[n(0)]
        credit: CreditKey,
        #[n(1)]
        refused: Option<String>,
    },
    // The payout plan flow: indices 43 to 59.
    /// The Eco intent a payout's order published was filled, as its destination Portal shows
    /// it at `block` (read by every node, past the route's deadline): the payout is paid.
    #[n(43)]
    PayoutDelivered {
        #[n(0)]
        id: u64,
        #[n(1)]
        block: BlockNumber,
    },
    /// The Eco intent a payout's order published never filled, as its destination Portal
    /// shows it at `block`, whose time is past the route's deadline (read by every node): the
    /// payout is undone next, its USDC owed back by Eco.
    #[n(44)]
    PayoutUnfilled {
        #[n(0)]
        id: u64,
        #[n(1)]
        block: BlockNumber,
    },
    /// A controller resolved a payout stopped for a human (`resolve_payout`): `Refund` puts
    /// it back on its undo (an ICP swap's refund asked next, under `refund_at`, the dedup
    /// instant the door chose; a withdrawal's shares minted back with `PayoutUndone` next, in
    /// the same call), `MarkPaid` books its paid line, once.
    #[n(45)]
    PayoutResolved {
        #[n(0)]
        id: u64,
        #[n(1)]
        resolution: PayoutResolution,
        #[n(2)]
        refund_at: Option<Timestamp>,
    },
    /// A planned swap's plan was tried and waits, with why (a scaler read that failed, a
    /// check that refused it): tried again while it can still fit, at most
    /// `MAX_FIT_TRIES` times in all.
    #[n(46)]
    PayoutFitWaited {
        #[n(0)]
        id: u64,
        #[n(1)]
        why: String,
    },
    /// A delivering payout's Eco intent's fill was read too early: no block every node
    /// agreed on was past the route's deadline yet. Not read again within one wait.
    #[n(47)]
    PayoutFillEarly {
        #[n(0)]
        id: u64,
    },
    /// A controller resolved a deposit held for a human (`resolve_deposit`), once: `Retry`
    /// puts it back on its payout with fresh attempts, a started one whose transfer may have
    /// landed with its own dedup pair (`retry_at` none), one whose transfer cannot land twice
    /// asked afresh at `retry_at`, the door's instant; `Refund`, when nothing of its payout
    /// went out, holds what arrived as an orphan under its transfer's key, for its payer;
    /// `MarkPaid` books its transfer as asked paid in the block named. The lines the
    /// intermediate builds of 1649a9f..026e392 wrote were never replayed anywhere (never
    /// deployed): no rule of theirs is kept for them.
    #[n(48)]
    DepositResolved {
        #[n(0)]
        r#ref: PoolRef,
        #[n(1)]
        resolution: DepositResolution,
        #[n(2)]
        retry_at: Option<Timestamp>,
    },
    /// A controller resolved an ICP stake whose pull's answer was lost past the ledger's
    /// deduplication window (`resolve_stake`), once.
    #[n(49)]
    StakeResolved {
        #[n(0)]
        id: PoolOpId,
        #[n(1)]
        resolution: StakeResolution,
    },
    /// A payout's transfer to its recipient is asked next: `amount`
    /// of `token`, spending `spent` of the ICP pocket once paid. From here it may have
    /// landed, until the ledger refuses it for sure.
    #[n(50)]
    TransferAsked {
        #[n(0)]
        r#ref: PoolRef,
        #[n(1)]
        token: PrincipalBytes,
        #[n(2)]
        amount: TokenAmount,
        #[n(3)]
        spent: TokenAmount,
    },
    /// The ledger refused a payout's transfer as asked, for sure: nothing of that ask landed.
    #[n(51)]
    TransferRefused {
        #[n(0)]
        r#ref: PoolRef,
    },
    /// An answer of a payout's transfer was lost (unknown, or too old to tell): it may have
    /// landed, whatever comes after. `held`, why: on a ledger the
    /// canister does not know to deduplicate, never asked again, the payout held for a human
    /// at once.
    #[n(52)]
    TransferLost {
        #[n(0)]
        r#ref: PoolRef,
        #[n(1)]
        held: Option<String>,
    },
    // the rebalancing: indices 60 to 89.
    /// A controller set the rebalancing's knobs.
    #[n(60)]
    MoveSettingsSet {
        #[n(0)]
        settings: RebalanceSettings,
    },
    /// The canister opened a move from its books: its amount reserved in its source pocket.
    #[n(61)]
    MoveOpened {
        #[n(0)]
        id: u64,
        #[n(1)]
        direction: MoveDirection,
        #[n(2)]
        amount: TokenAmount,
        #[n(3)]
        urgent: bool,
        /// The daily cap is read at this instant, the step's own.
        #[n(4)]
        opened_at: Timestamp,
        #[n(5)]
        window_ends: Option<Timestamp>,
        #[n(6)]
        own_cost: TokenAmount,
    },
    /// The watcher proposed a route for the move that passed every check, cheaper than any
    /// before it: pinned as the move's proposal.
    #[n(62)]
    MoveProposed {
        #[n(0)]
        id: u64,
        #[n(1)]
        order: PinnedOrder,
        #[n(2)]
        fee: TokenAmount,
    },
    /// The move's route is decided and its order pinned: the cheapest proposal, or the
    /// canister's own.
    #[n(63)]
    MoveRouted {
        #[n(0)]
        id: u64,
        #[n(1)]
        route: MoveRoute,
        #[n(2)]
        order: PinnedOrder,
        /// Base to ICP by Eco: the Ethereum block its fill is looked for from.
        #[n(3)]
        scan_from: Option<BlockNumber>,
    },
    /// The canister signed a transaction of the move (its order's `run`, or a ck deposit):
    /// the exact bytes, recorded before they are broadcast, spending the nonce `TxCreated`
    /// handed out for it. Its first order leaves the source pocket (Base to ICP).
    #[n(64)]
    MoveSigned {
        #[n(0)]
        id: u64,
        #[n(1)]
        chain_id: ChainId,
        #[n(2)]
        nonce: Nonce,
        #[n(3)]
        tx_hash: TxHash,
        #[cbor(n(4), with = "minicbor::bytes")]
        raw_tx: Vec<u8>,
    },
    /// Its transaction succeeded at depth, with the Eco intent it published, if any.
    #[n(65)]
    MoveBurned {
        #[n(0)]
        id: u64,
        #[n(1)]
        tx_hash: TxHash,
        #[n(2)]
        block: BlockNumber,
        #[cbor(n(3), with = "minicbor::bytes")]
        intent: Option<[u8; 32]>,
    },
    /// Its transaction reverted at depth: back to signing the same order, and a Base to ICP
    /// move's first order back to reserved.
    #[n(66)]
    MoveReverted {
        #[n(0)]
        id: u64,
        /// The block its transaction reverted in: `Swapic.used` is read at it or after.
        #[n(1)]
        block: Option<BlockNumber>,
    },
    /// ICP to Base: the ck minter's withdrawal is about to be asked, its ckUSDC in flight from
    /// now; `created_at` dedups its approvals.
    #[n(67)]
    MoveWithdrawing {
        #[n(0)]
        id: u64,
        #[n(1)]
        created_at: Timestamp,
    },
    /// ICP to Base: the minter took the ckUSDC (its withdrawal id); `fee` is the ckUSDC its
    /// approval cost the pool.
    #[n(68)]
    MoveWithdrawn {
        #[n(0)]
        id: u64,
        #[n(1)]
        withdrawal_id: u64,
        #[n(2)]
        fee: TokenAmount,
    },
    /// ICP to Base: the minter (or an approval) refused: nothing left, back to reserved;
    /// `fee` is what an approval made before it cost.
    #[n(69)]
    MoveRefused {
        #[n(0)]
        id: u64,
        #[n(1)]
        why: String,
        #[n(2)]
        fee: TokenAmount,
    },
    /// Its USDC reached the canister's Ethereum address, read at the crediting depth: an ICP
    /// to Base move's from the minter (its window, if any, from now), a Base to ICP move's
    /// Eco fill.
    #[n(70)]
    MoveLanded {
        #[n(0)]
        id: u64,
        #[n(1)]
        tx_hash: TxHash,
        #[n(2)]
        block: BlockNumber,
        #[n(3)]
        amount: TokenAmount,
        #[n(4)]
        window_ends: Option<Timestamp>,
    },
    /// Base to ICP: the sweep of the rebalance subaccount is about to be asked: its dedup
    /// instant, what it moves, and what the move is credited (never above its amount less its
    /// route's cost: a donation is never the pool's).
    #[n(71)]
    MoveSweeping {
        #[n(0)]
        id: u64,
        #[n(1)]
        created_at: Timestamp,
        #[n(2)]
        amount: TokenAmount,
        #[n(3)]
        credited: TokenAmount,
    },
    /// The move arrived: its destination credited, once. On Base by the transfer's credit
    /// key (and its block); on ICP by the sweep's ledger block.
    #[n(72)]
    MoveArrived {
        #[n(0)]
        id: u64,
        #[n(1)]
        credited: TokenAmount,
        #[n(2)]
        credit: Option<CreditKey>,
        #[n(3)]
        block: Option<BlockNumber>,
        #[n(4)]
        ledger_block: Option<u64>,
    },
    /// The move stopped for a controller, held where it is.
    #[n(73)]
    MoveStuck {
        #[n(0)]
        id: u64,
        #[n(1)]
        why: String,
    },
    /// A controller sent the move on from where it stood.
    #[n(74)]
    MoveRetried {
        #[n(0)]
        id: u64,
    },
    /// A controller wrote the move off.
    #[n(75)]
    MoveLost {
        #[n(0)]
        id: u64,
        #[n(1)]
        why: String,
    },
    /// Base to ICP by Eco: the intent went unfilled and Eco refunded its reward, the move's
    /// whole amount, to the canister's Base address: reserved again, routed again.
    #[n(76)]
    MoveRefunded {
        #[n(0)]
        id: u64,
        #[n(1)]
        credit: CreditKey,
        #[n(2)]
        amount: TokenAmount,
        #[n(3)]
        block: BlockNumber,
    },
    // the Eco rescues and refunds: indices 77 and up.
    /// The canister signed Eco's `cancelAndProve` of an unfilled intent on its destination,
    /// the intent published by the order `order_id` (a relay's, a payout's or a move's): the
    /// exact bytes, recorded before they are broadcast, spending the nonce `TxCreated` handed
    /// out for it. Eco refunds the reward's creator on the source by itself.
    #[n(77)]
    EcoRescueSigned {
        #[cbor(n(0), with = "minicbor::bytes")]
        order_id: [u8; 32],
        #[n(1)]
        chain_id: ChainId,
        #[n(2)]
        nonce: Nonce,
        #[n(3)]
        tx_hash: TxHash,
        #[cbor(n(4), with = "minicbor::bytes")]
        raw_tx: Vec<u8>,
    },
    /// ICP to Base by Eco: the destination Portal shows the intent never filled and the source
    /// Portal its reward refunded, so the move's USDC is back at the canister's Ethereum
    /// address: landed again, routed again.
    #[n(78)]
    MoveUnfilled {
        #[n(0)]
        id: u64,
    },
    /// ICP to Base: the ck minter reimbursed the withdrawal, `amount` of ckUSDC back at the
    /// canister's account in its ledger `block`: back in the ICP pocket, the move
    /// closed.
    #[n(79)]
    MoveReimbursed {
        #[n(0)]
        id: u64,
        #[n(1)]
        amount: TokenAmount,
        #[n(2)]
        block: u64,
    },
    /// A controller's EVM sweep `id` was signed: its transaction's bytes. Nothing of the
    /// pool moves; the sweep's own line took its amount off the books.
    #[n(80)]
    SweepSigned {
        #[n(0)]
        id: u64,
        #[n(1)]
        chain_id: ChainId,
        #[n(2)]
        nonce: Nonce,
        #[n(3)]
        tx_hash: TxHash,
        #[cbor(n(4), with = "minicbor::bytes")]
        raw_tx: Vec<u8>,
    },
    // 81 and 82 are reserved for the cancelled EVM to ICP token route: never reused.
    /// A ck payout's withdrawal begins: `icp.reserved` and `owed` fall by its hold before any
    /// of it leaves, so the hourly check sees a surplus, and what it burns and the ledger's
    /// fee are fixed for every retry.
    #[n(83)]
    PayoutCkWithdrawing {
        #[n(0)]
        id: u64,
        #[n(1)]
        created_at: Timestamp,
        /// What the minter burns.
        #[n(2)]
        amount: TokenAmount,
        #[n(3)]
        fee: TokenAmount,
    },
    /// The minter holds the ckUSDC of its withdrawal `withdrawal_id`: by its answer, or by
    /// `BySenderAccount` after a lost one.
    #[n(84)]
    PayoutCkWithdrawn {
        #[n(0)]
        id: u64,
        #[n(1)]
        withdrawal_id: u64,
    },
    /// Its USDC was paid on Ethereum: the minter's `TxFinalized` success, or a controller's
    /// word (`resolve_ck_withdrawal`). No line moves: they fell at its withdrawal.
    #[n(85)]
    PayoutCkLanded {
        #[n(0)]
        id: u64,
        #[n(1)]
        tx_hash: TxHash,
        #[n(2)]
        by_controller: bool,
    },
    /// The way back of a ck payout, on proof only: the minter reimbursed it (`burned`), or
    /// nothing was burned. Adds back exactly what `PayoutCkWithdrawing` took and begins its
    /// undo, so the hourly check leaves its refund out while the ckUSDC comes home.
    #[n(86)]
    PayoutCkReimbursed {
        #[n(0)]
        id: u64,
        /// What the burn subaccount holds to bring back ([`CkBack::left`]).
        #[n(1)]
        left: TokenAmount,
        /// The ledger block of a reimbursement.
        #[n(2)]
        block: Option<u64>,
        #[n(3)]
        burned: bool,
    },
    // 87 is reserved for the cancelled EVM to ICP token route: never reused.
}

impl PoolStep {
    pub fn to_bytes(&self) -> Vec<u8> {
        minicbor::to_vec(self).expect("BUG: encoding into a Vec is infallible")
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        minicbor::decode(bytes).ok()
    }

    pub fn name(&self) -> &'static str {
        match self {
            PoolStep::SettingsSet { .. } => "SettingsSet",
            PoolStep::Registered { .. } => "Registered",
            PoolStep::Expired { .. } => "Expired",
            PoolStep::Arrived { .. } => "Arrived",
            PoolStep::Unattributed { .. } => "Unattributed",
            PoolStep::PayoutStarted { .. } => "PayoutStarted",
            PoolStep::Paid { .. } => "Paid",
            PoolStep::FellBack { .. } => "FellBack",
            PoolStep::Minted { .. } => "Minted",
            PoolStep::StakedIcp { .. } => "StakedIcp",
            PoolStep::WithdrawnIcp { .. } => "WithdrawnIcp",
            PoolStep::AttemptFailed { .. } => "AttemptFailed",
            PoolStep::SwapDeclined { .. } => "SwapDeclined",
            PoolStep::SwapBegun { .. } => "SwapBegun",
            PoolStep::Swapped { .. } => "Swapped",
            PoolStep::StakeStarted { .. } => "StakeStarted",
            PoolStep::StakeFailed { .. } => "StakeFailed",
            PoolStep::WithdrawStarted { .. } => "WithdrawStarted",
            PoolStep::IcpChecked { .. } => "IcpChecked",
            PoolStep::SwapApproved { .. } => "SwapApproved",
            PoolStep::SwapUndone { .. } => "SwapUndone",
            PoolStep::PayoutSettingsSet { .. } => "PayoutSettingsSet",
            PoolStep::PayoutOpened { .. } => "PayoutOpened",
            PoolStep::PayoutAmountKnown { .. } => "PayoutAmountKnown",
            PoolStep::PayoutQueued { .. } => "PayoutQueued",
            PoolStep::PayoutWaiting { .. } => "PayoutWaiting",
            PoolStep::PayoutSigned { .. } => "PayoutSigned",
            PoolStep::PayoutPaid { .. } => "PayoutPaid",
            PoolStep::PayoutReverted { .. } => "PayoutReverted",
            PoolStep::PayoutUndoing { .. } => "PayoutUndoing",
            PoolStep::PayoutUndone { .. } => "PayoutUndone",
            PoolStep::IcpRoutesSet { .. } => "IcpRoutesSet",
            PoolStep::ApproveSigned { .. } => "ApproveSigned",
            PoolStep::SweepSigned { .. } => "SweepSigned",
            PoolStep::PayoutPulled { .. } => "PayoutPulled",
            PoolStep::PayoutStopped { .. } => "PayoutStopped",
            PoolStep::PayoutRelisted { .. } => "PayoutRelisted",
            PoolStep::PayoutRefunded { .. } => "PayoutRefunded",
            PoolStep::PayoutDelivered { .. } => "PayoutDelivered",
            PoolStep::PayoutUnfilled { .. } => "PayoutUnfilled",
            PoolStep::PayoutResolved { .. } => "PayoutResolved",
            PoolStep::PayoutFitWaited { .. } => "PayoutFitWaited",
            PoolStep::PayoutFillEarly { .. } => "PayoutFillEarly",
            PoolStep::DepositResolved { .. } => "DepositResolved",
            PoolStep::StakeResolved { .. } => "StakeResolved",
            PoolStep::TransferAsked { .. } => "TransferAsked",
            PoolStep::TransferRefused { .. } => "TransferRefused",
            PoolStep::TransferLost { .. } => "TransferLost",
            PoolStep::OrphanBooked { .. } => "OrphanBooked",
            PoolStep::OrphanRefundSigned { .. } => "OrphanRefundSigned",
            PoolStep::OrphanRefundClosed { .. } => "OrphanRefundClosed",
            PoolStep::TermsClaimed { .. } => "TermsClaimed",
            PoolStep::MoveSettingsSet { .. } => "MoveSettingsSet",
            PoolStep::MoveOpened { .. } => "MoveOpened",
            PoolStep::MoveProposed { .. } => "MoveProposed",
            PoolStep::MoveRouted { .. } => "MoveRouted",
            PoolStep::MoveSigned { .. } => "MoveSigned",
            PoolStep::MoveBurned { .. } => "MoveBurned",
            PoolStep::MoveReverted { .. } => "MoveReverted",
            PoolStep::MoveWithdrawing { .. } => "MoveWithdrawing",
            PoolStep::MoveWithdrawn { .. } => "MoveWithdrawn",
            PoolStep::MoveRefused { .. } => "MoveRefused",
            PoolStep::MoveLanded { .. } => "MoveLanded",
            PoolStep::MoveSweeping { .. } => "MoveSweeping",
            PoolStep::MoveArrived { .. } => "MoveArrived",
            PoolStep::MoveStuck { .. } => "MoveStuck",
            PoolStep::MoveRetried { .. } => "MoveRetried",
            PoolStep::MoveLost { .. } => "MoveLost",
            PoolStep::MoveRefunded { .. } => "MoveRefunded",
            PoolStep::EcoRescueSigned { .. } => "EcoRescueSigned",
            PoolStep::MoveUnfilled { .. } => "MoveUnfilled",
            PoolStep::MoveReimbursed { .. } => "MoveReimbursed",
            PoolStep::PayoutCkWithdrawing { .. } => "PayoutCkWithdrawing",
            PoolStep::PayoutCkWithdrawn { .. } => "PayoutCkWithdrawn",
            PoolStep::PayoutCkLanded { .. } => "PayoutCkLanded",
            PoolStep::PayoutCkReimbursed { .. } => "PayoutCkReimbursed",
        }
    }

    /// Whether the step gives a payout something to do now: the engine drives at once.
    /// The transaction a step records the exact bytes of, if it records one: its chain, its
    /// nonce, its hash and its bytes.
    pub fn signed_tx(&self) -> Option<(ChainId, Nonce, TxHash, &[u8])> {
        match self {
            PoolStep::PayoutSigned {
                chain_id,
                nonce,
                tx_hash,
                raw_tx,
                ..
            }
            | PoolStep::ApproveSigned {
                chain_id,
                nonce,
                tx_hash,
                raw_tx,
            }
            | PoolStep::SweepSigned {
                chain_id,
                nonce,
                tx_hash,
                raw_tx,
                ..
            }
            | PoolStep::OrphanRefundSigned {
                chain_id,
                nonce,
                tx_hash,
                raw_tx,
                ..
            }
            | PoolStep::MoveSigned {
                chain_id,
                nonce,
                tx_hash,
                raw_tx,
                ..
            }
            | PoolStep::EcoRescueSigned {
                chain_id,
                nonce,
                tx_hash,
                raw_tx,
                ..
            } => Some((*chain_id, *nonce, *tx_hash, raw_tx)),
            _ => None,
        }
    }

    /// The purpose of the transaction a step records the bytes of: the nonce it spends must
    /// have been handed out for exactly this.
    pub fn signed_purpose(&self) -> Option<crate::events::TxPurpose> {
        match self {
            PoolStep::PayoutSigned { id, .. } => Some(crate::events::TxPurpose::PoolPayout(*id)),
            PoolStep::ApproveSigned { .. } => Some(crate::events::TxPurpose::PoolApprove),
            PoolStep::SweepSigned { id, .. } => Some(crate::events::TxPurpose::Sweep(*id)),
            PoolStep::OrphanRefundSigned { key, .. } => {
                Some(crate::events::TxPurpose::PoolTransfer(orphan_key(key)))
            }
            PoolStep::MoveSigned { id, .. } => Some(crate::events::TxPurpose::PoolMove(*id)),
            PoolStep::EcoRescueSigned { order_id, .. } => {
                Some(crate::events::TxPurpose::EcoRescue(*order_id))
            }
            _ => None,
        }
    }

    pub fn moves_a_payout(&self) -> bool {
        matches!(
            self,
            PoolStep::Arrived { .. }
                | PoolStep::PayoutStarted { .. }
                | PoolStep::FellBack { .. }
                | PoolStep::Swapped { .. }
                | PoolStep::StakeStarted { .. }
                | PoolStep::WithdrawStarted { .. }
                // the Base payouts: an ICP swap's next step and USDC freed on Base for the
                // queue. A revert's retry waits for the backstop, so an order that keeps
                // reverting is signed at most once a backstop interval
                | PoolStep::PayoutPulled { .. }
                | PoolStep::PayoutUndoing { .. }
                | PoolStep::PayoutUndone { .. }
                | PoolStep::PayoutRefunded { .. }
                | PoolStep::PayoutUnfilled { .. }
                | PoolStep::PayoutResolved { .. }
                | PoolStep::PayoutCkReimbursed { .. }
                | PoolStep::Minted { .. }
                // a deposit retried pays again; one refunded frees ckUSDC for the queue
                | PoolStep::DepositResolved { .. }
        )
    }
}
