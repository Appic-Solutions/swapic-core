use super::*;
use crate::ic::{CallError, Dedup};
use crate::storage::on_fresh_memory;
use crate::storage::pool::tests::{
    account, at, base_stake, ckusdc, funded_pool, sign_payout, usdc, waiting_withdrawal, FEE,
};
use crate::storage::pool::{ledger, payout, record_at, shares_of};
use candid::Principal;
use std::cell::RefCell;
use std::time::Duration;
use types::pool::payout::{IcpSwapTerms, PayoutKind, PayoutTerms, PinnedOrder, Undo};
use types::pool::{IcpAccount, PoolAccount};
use types::{ChainId, EvmAddress, TokenAmount};

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    match future.as_mut().poll(&mut context) {
        std::task::Poll::Ready(out) => out,
        std::task::Poll::Pending => panic!("the fake ledger never waits"),
    }
}

fn open_pool() -> Result<(), String> {
    Ok(())
}

fn staker() -> EvmAddress {
    "0x1111111111111111111111111111111111111111"
        .parse()
        .unwrap()
}

fn terms(deadline: Timestamp) -> PayoutTerms {
    PayoutTerms {
        dst_chain: ChainId::BASE,
        recipient: staker(),
        token_out: staker(),
        min_out: TokenAmount::ONE,
        deadline,
    }
}

fn payout_in(status: PayoutStatus, deadline: Timestamp) -> EvmPayout {
    let mut payout = EvmPayout::new(
        1,
        PayoutKind::IcpSwap(IcpSwapTerms {
            token_in: ckusdc(),
            amount_in: usdc(100),
            from: account(5),
            min_usdc: usdc(100),
            premium: usdc(1),
            network_fee: TokenAmount::from(50_000_u32),
            refund_to: account(6),
            slippage_bps: None,
            planned: None,
        }),
        terms(deadline),
        Undo::Refund { to: account(6) },
        at(0),
    );
    payout.status = status;
    payout.waiting_since = Some(at(0));
    payout
}

fn secs(seconds: u64) -> Timestamp {
    Timestamp::from_secs(seconds).unwrap()
}

/// The pass's decisions, at the deadline and before it: never an undo while signed.
#[test]
fn the_pass_decides_by_the_payouts_place_and_its_deadline() {
    let settings = PayoutSettings::default();
    let deadline = secs(10_000);
    let before = secs(1_000);
    let past = secs(10_000);
    let decide = |payout: &EvmPayout, now, in_flight| next(payout, &settings, now, in_flight);
    // queued: waits, and is undone at its deadline
    let queued = payout_in(PayoutStatus::Queued, deadline);
    assert_eq!(decide(&queued, before, false), Next::Wait);
    assert!(matches!(
        decide(&queued, past, false),
        Next::Undo { checked: false, .. }
    ));
    // waiting with no order: listed again after `order_wait`, undone at its deadline
    let waiting = payout_in(PayoutStatus::Waiting, deadline);
    assert_eq!(decide(&waiting, secs(60), false), Next::Wait);
    assert_eq!(decide(&waiting, secs(120), false), Next::Relist);
    assert!(matches!(
        decide(&waiting, past, false),
        Next::Undo { checked: false, .. }
    ));
    // signed: never undone, whatever the clock
    let signed = payout_in(PayoutStatus::Signed(7), deadline);
    assert_eq!(decide(&signed, past, false), Next::Wait);
    // reverted, its order pinned: never signed again; undone at once, whatever the clock,
    // only once Swapic says it never ran
    let mut reverted = payout_in(PayoutStatus::Reverted, deadline);
    reverted.order = Some(PinnedOrder {
        order_id: [1; 32],
        order: vec![1],
        gas_limit: 300_000,
        deadline_s: 2_000,
    });
    for now in [before, past] {
        assert_eq!(
            decide(&reverted, now, false),
            Next::Undo {
                why: REVERTED.to_string(),
                checked: true
            }
        );
        // its transaction still in the outbox (until the crediting depth): nothing
        assert_eq!(decide(&reverted, now, true), Next::Wait);
    }
    // delivering into an Eco intent: its fill read once its route can no longer be filled
    // (an order that does not read: the latest any route may run, an hour after it)
    let mut delivering = payout_in(PayoutStatus::Delivering, deadline);
    delivering.intent = Some([2; 32]);
    delivering.order = reverted.order.clone();
    let after_s = 2_000 + crate::pool::eco::MAX_ROUTE_LIFE_S;
    assert_eq!(fill_read_after(&delivering), after_s);
    assert_eq!(
        decide(&delivering, secs(after_s + FILL_MARGIN_S - 1), false),
        Next::Wait
    );
    assert_eq!(
        decide(&delivering, secs(after_s + FILL_MARGIN_S), false),
        Next::ReadFill {
            chain: ChainId::BASE,
            intent: [2; 32],
            after_s
        }
    );
    // never filled: undone at once
    let unfilled = payout_in(PayoutStatus::Unfilled, deadline);
    assert_eq!(
        decide(&unfilled, before, false),
        Next::Undo {
            why: UNFILLED.to_string(),
            checked: false
        }
    );
    // an ICP swap on its way is driven; one whose pull is too old to ask again stops
    let opened = payout_in(PayoutStatus::Opened, deadline);
    assert_eq!(decide(&opened, before, false), Next::Drive);
    let old = at(0).checked_add(DEDUP_WINDOW).unwrap();
    assert!(matches!(decide(&opened, old, false), Next::Stop(_)));
}

/// Review Focus 4: the queue is served first in first out, the moment the pocket covers its
/// head, and nothing moves ahead of it.
#[test]
fn the_queue_is_served_in_order_when_the_pocket_refills() {
    on_fresh_memory(|| {
        crate::storage::init();
        let id = waiting_withdrawal(staker());
        // the staker also stakes 1,000 on ICP: its value is above what the Base pocket holds
        let op = types::hash::PoolOpId::new([0xef; 32]);
        let owner = PoolAccount::Evm(staker());
        record_at(
            PoolStep::StakeStarted {
                id: op,
                owner,
                from: account(9),
                amount: usdc(1_000),
                created_at: at(4),
            },
            at(4),
        )
        .unwrap();
        let minted = ledger().mint_now(usdc(1_000)).unwrap();
        record_at(
            PoolStep::StakedIcp {
                id: op,
                owner,
                amount: usdc(1_000),
                rebalance_share: TokenAmount::ZERO,
                shares: minted,
                block: 4,
            },
            at(4),
        )
        .unwrap();
        // two more payouts, opened and known, the Base pocket short of the first
        let base_free = ledger().pockets.base.free.checked_add(usdc(50)).unwrap();
        let withdrawal = |nonce: u64, amount: TokenAmount| {
            let pool = ledger();
            let burned =
                types::pool::Shares::burn_for(amount, pool.total_assets(), pool.total_shares)
                    .unwrap();
            types::pool::EvmWithdrawal {
                staker: staker(),
                usdc: amount,
                max_shares: burned,
                rebalance_share: usdc(1),
                network_fee: TokenAmount::from(50_000_u32),
                nonce,
                burned,
            }
        };
        let mut queued = Vec::new();
        for (nonce, amount) in [(2, base_free), (3, usdc(1).checked_mul(5_u8).unwrap())] {
            let w = withdrawal(nonce, amount);
            let next_id = ledger().lines().next_id;
            for step in [
                PoolStep::PayoutOpened {
                    id: next_id,
                    kind: Box::new(PayoutKind::Withdrawal(w)),
                    terms: terms(at(1_000_000_000_000)),
                    undo: Undo::Remint {
                        owner: PoolAccount::Evm(staker()),
                        shares: w.burned,
                    },
                    plan: None,
                },
                PoolStep::PayoutAmountKnown {
                    id: next_id,
                    amount: w.payout_amount().unwrap(),
                    received: None,
                },
            ] {
                record_at(step, at(5)).unwrap();
            }
            icp_swap::place(next_id, at(5)).unwrap();
            queued.push(next_id);
        }
        // the first is too large for what is free; the second could wait, but queues behind it
        assert_eq!(payout(queued[0]).unwrap().status, PayoutStatus::Queued);
        assert_eq!(payout(queued[1]).unwrap().status, PayoutStatus::Queued);
        assert_eq!(serve(at(6)), 0);
        // the waiting payout is paid out... no: the pocket refills with a stake on Base
        let _ = id;
        base_stake(staker(), usdc(400), 0x78, 2);
        assert_eq!(serve(at(7)), 2);
        assert_eq!(payout(queued[0]).unwrap().status, PayoutStatus::Waiting);
        assert_eq!(payout(queued[1]).unwrap().status, PayoutStatus::Waiting);
        assert_eq!(ledger().lines().queue_len, 0);
    });
}

/// A ledger that executes each transfer once.
#[derive(Default)]
struct Fake {
    transfers: RefCell<Vec<(Principal, IcpAccount, TokenAmount)>>,
}

/// No plan is fitted here: the drive's own tests fit them.
impl PlanReads for Fake {
    async fn scaled(&self, _route: &[u8], _amount: TokenAmount) -> Result<Vec<u8>, String> {
        Err("no scaler here".to_string())
    }

    fn checked(
        &self,
        _payout: &EvmPayout,
        _order: &types::swapic::Order,
        _order_bytes: &[u8],
        _gas_limit: u64,
        _now: Timestamp,
    ) -> Result<PinnedOrder, settlement_api::types::pool::PoolError> {
        Err(settlement_api::types::pool::PoolError::Unavailable(
            "no checks here".to_string(),
        ))
    }
}

impl Ledgers for Fake {
    async fn transfer(
        &self,
        ledger: Principal,
        to: IcpAccount,
        amount: TokenAmount,
        _dedup: Dedup,
    ) -> Result<u64, CallError> {
        self.transfers.borrow_mut().push((ledger, to, amount));
        Ok(500)
    }

    async fn pull(
        &self,
        _ledger: Principal,
        _from: IcpAccount,
        _amount: TokenAmount,
        _dedup: Dedup,
    ) -> Result<u64, CallError> {
        Ok(7)
    }

    async fn approve(
        &self,
        _ledger: Principal,
        _spender: Principal,
        _amount: TokenAmount,
    ) -> Result<(), CallError> {
        Ok(())
    }

    async fn direction(
        &self,
        _pool: Principal,
        _token_in: Principal,
        _token_out: Principal,
    ) -> Result<Option<bool>, CallError> {
        Ok(Some(true))
    }

    async fn quote(
        &self,
        _pool: Principal,
        _amount_in: TokenAmount,
        _zero_for_one: bool,
    ) -> Result<TokenAmount, CallError> {
        Ok(TokenAmount::ZERO)
    }

    async fn swap(
        &self,
        _pool: Principal,
        _amount_in: TokenAmount,
        _zero_for_one: bool,
        _min_out: TokenAmount,
        _fee_in: TokenAmount,
        _fee_out: TokenAmount,
    ) -> Result<TokenAmount, CallError> {
        Err(CallError::Refused("no swap here".into()))
    }

    async fn unused(&self, _pool: Principal) -> Result<(TokenAmount, TokenAmount), CallError> {
        Ok((TokenAmount::ZERO, TokenAmount::ZERO))
    }

    async fn withdraw(
        &self,
        _pool: Principal,
        _token: Principal,
        _amount: TokenAmount,
        _fee: TokenAmount,
    ) -> Result<(), CallError> {
        Ok(())
    }
}

/// Review Focus 5: an ICP swap that cannot be paid by its deadline refunds all the ckUSDC it
/// produced, less the ledger's fee, to `refund_to`; a withdrawal mints back exactly the
/// shares it burned; every line is back where it was; a signed one is never undone.
#[test]
fn an_undo_at_the_deadline_is_exact() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        record_at(
            PoolStep::PayoutSettingsSet {
                settings: PayoutSettings::default(),
            },
            at(1),
        )
        .unwrap();
        record_at(
            PoolStep::SettingsSet {
                settings: types::pool::PoolSettings {
                    open_for_swaps: true,
                    ledger_fees: vec![types::pool::LedgerFee {
                        ledger: ckusdc(),
                        fee: TokenAmount::from(FEE),
                    }],
                    ..types::pool::PoolSettings::default()
                },
            },
            at(1),
        )
        .unwrap();
        let before = ledger();
        // an ICP swap of 100 ckUSDC, queued: the Base pocket is empty
        let deadline = at(1_000);
        let id = ledger().lines().next_id;
        let swap = IcpSwapTerms {
            token_in: ckusdc(),
            amount_in: usdc(100),
            from: account(5),
            min_usdc: usdc(100),
            premium: usdc(1),
            network_fee: TokenAmount::from(50_000_u32),
            refund_to: account(6),
            slippage_bps: None,
            planned: None,
        };
        record_at(
            PoolStep::PayoutOpened {
                id,
                kind: Box::new(PayoutKind::IcpSwap(swap)),
                terms: terms(deadline),
                undo: Undo::Refund { to: account(6) },
                plan: None,
            },
            at(2),
        )
        .unwrap();
        let fake = Fake::default();
        block_on(icp_swap::drive(&fake, id, || at(3), &open_pool)).unwrap();
        assert_eq!(payout(id).unwrap().status, PayoutStatus::Queued);
        // before its deadline: nothing undone
        block_on(pass(&fake, || at(500), &open_pool));
        assert_eq!(payout(id).unwrap().status, PayoutStatus::Queued);
        // at its deadline: all its ckUSDC back, less the ledger's fee
        block_on(pass(&fake, || deadline, &open_pool));
        assert_eq!(
            *fake.transfers.borrow(),
            vec![(
                ckusdc(),
                account(6),
                usdc(100).checked_sub(TokenAmount::from(FEE)).unwrap()
            )]
        );
        let undone = payout(id).unwrap();
        assert!(matches!(undone.status, PayoutStatus::Undone(_)));
        assert_eq!(undone.icp_block, Some(500));
        let after = ledger();
        assert_eq!(after.pockets, before.pockets);
        assert_eq!(after.owed, before.owed);
        assert_eq!(after.lines().fees_icp, TokenAmount::ZERO);
        assert_eq!(after.lines().queue_len, 0);
        assert_eq!(after.total_assets(), before.total_assets());
    });
}

/// A withdrawal undone at its deadline mints back exactly the shares it burned; a signed one
/// waits for its transaction, whatever the clock says.
#[test]
fn a_withdrawal_is_reminted_at_its_deadline_and_a_signed_one_is_not_undone() {
    on_fresh_memory(|| {
        crate::storage::init();
        let id = waiting_withdrawal(staker());
        let owner = PoolAccount::Evm(staker());
        let burned = match payout(id).unwrap().undo {
            Undo::Remint { shares, .. } => shares,
            Undo::Refund { .. } => unreachable!(),
        };
        let shares_now = shares_of(&owner);
        let fake = Fake::default();
        let deadline = payout(id).unwrap().terms.deadline;
        // signed: past its deadline it still waits for its transaction
        sign_payout(id, 0, 9);
        block_on(pass(&fake, || deadline, &open_pool));
        assert!(matches!(
            payout(id).unwrap().status,
            PayoutStatus::Signed(_)
        ));
        // reverted with its order pinned, Swapic's `used` not read: nothing is undone
        record_at(PoolStep::PayoutReverted { id }, at(10)).unwrap();
        block_on(pass_with(&fake, &Reads::unread(), || deadline, &open_pool));
        assert_eq!(payout(id).unwrap().status, PayoutStatus::Reverted);
        assert_eq!(shares_of(&owner), shares_now);
        // a withdrawal never signed is undone at its deadline: exactly its shares back
        let other = staker_two();
        let second = waiting_withdrawal_again(other);
        let burned_two = match payout(second).unwrap().undo {
            Undo::Remint { shares, .. } => shares,
            Undo::Refund { .. } => unreachable!(),
        };
        let owner_two = PoolAccount::Evm(other);
        let before = shares_of(&owner_two);
        block_on(pass_with(&fake, &Reads::unread(), || deadline, &open_pool));
        assert!(matches!(
            payout(second).unwrap().status,
            PayoutStatus::Undone(_)
        ));
        assert_eq!(
            shares_of(&owner_two),
            before.checked_add(burned_two).unwrap()
        );
        let _ = burned;
    });
}

fn staker_two() -> EvmAddress {
    "0x3333333333333333333333333333333333333333"
        .parse()
        .unwrap()
}

/// A second staker's 50 USDC on Base and its withdrawal of 10, waiting: the next payout.
fn waiting_withdrawal_again(staker: EvmAddress) -> u64 {
    base_stake(staker, usdc(50), 0x79, 3);
    let pool = ledger();
    let burned =
        types::pool::Shares::burn_for(usdc(10), pool.total_assets(), pool.total_shares).unwrap();
    let withdrawal = types::pool::EvmWithdrawal {
        staker,
        usdc: usdc(10),
        max_shares: burned,
        rebalance_share: TokenAmount::from(2_000_u32),
        network_fee: TokenAmount::from(50_000_u32),
        nonce: 1,
        burned,
    };
    let id = pool.lines().next_id;
    let deadline = payout(0).unwrap().terms.deadline;
    for step in [
        PoolStep::PayoutOpened {
            id,
            kind: Box::new(PayoutKind::Withdrawal(withdrawal)),
            terms: PayoutTerms {
                recipient: staker,
                ..terms(deadline)
            },
            undo: Undo::Remint {
                owner: PoolAccount::Evm(staker),
                shares: burned,
            },
            plan: None,
        },
        PoolStep::PayoutAmountKnown {
            id,
            amount: withdrawal.payout_amount().unwrap(),
            received: None,
        },
    ] {
        record_at(step, at(11)).unwrap();
    }
    icp_swap::place(id, at(11)).unwrap();
    let _ = Duration::ZERO;
    id
}

/// What the fake chains answer: Swapic's `used` for every order, and every Eco intent's
/// fill.
struct Reads {
    ran: Result<bool, String>,
    asked: RefCell<Vec<[u8; 32]>>,
    fill: Result<(Fill, BlockNumber), crate::agreed::FillRead>,
    fills: RefCell<Vec<(ChainId, [u8; 32], u64)>>,
}

impl Reads {
    fn ran(ran: bool) -> Self {
        Self {
            ran: Ok(ran),
            ..Self::unread()
        }
    }

    fn unread() -> Self {
        Self {
            ran: Err("not read".to_string()),
            asked: RefCell::default(),
            fill: Err(crate::agreed::FillRead::Unread("not read".to_string())),
            fills: RefCell::default(),
        }
    }

    fn fill(fill: Result<Fill, crate::agreed::FillRead>) -> Self {
        Self {
            fill: fill.map(|fill| (fill, BlockNumber::new(77))),
            ..Self::unread()
        }
    }
}

impl ChainReads for Reads {
    async fn ran(&self, order_id: [u8; 32]) -> Result<bool, String> {
        self.asked.borrow_mut().push(order_id);
        self.ran.clone()
    }

    async fn fill_after(
        &self,
        chain: ChainId,
        intent: [u8; 32],
        after_s: u64,
    ) -> Result<(Fill, BlockNumber), crate::agreed::FillRead> {
        self.fills.borrow_mut().push((chain, intent, after_s));
        self.fill.clone()
    }
}

/// Item 6 of the final review fixes: a reverted payout is never signed again. Once Swapic
/// says its order never ran it is undone at once, a withdrawal minted back exactly the
/// shares it burned, before its deadline; a second pass does nothing more.
#[test]
fn a_reverted_payout_is_undone_once_final_and_unused() {
    on_fresh_memory(|| {
        crate::storage::init();
        let id = waiting_withdrawal(staker());
        let owner = PoolAccount::Evm(staker());
        let burned = match payout(id).unwrap().undo {
            Undo::Remint { shares, .. } => shares,
            Undo::Refund { .. } => unreachable!(),
        };
        let before = shares_of(&owner);
        sign_payout(id, 0, 9);
        record_at(PoolStep::PayoutReverted { id }, at(10)).unwrap();
        let reads = Reads::ran(false);
        let fake = Fake::default();
        block_on(pass_with(&fake, &reads, || at(11), &open_pool));
        assert_eq!(*reads.asked.borrow(), vec![[9; 32]]);
        assert_eq!(
            payout(id).unwrap().status,
            PayoutStatus::Undone(REVERTED.to_string())
        );
        assert_eq!(shares_of(&owner), before.checked_add(burned).unwrap());
        // nothing signed, nothing undone twice
        assert!(read_state(|state| state.unsigned_nonces()).is_empty());
        block_on(pass_with(&fake, &reads, || at(12), &open_pool));
        assert_eq!(shares_of(&owner), before.checked_add(burned).unwrap());
        assert_eq!(reads.asked.borrow().len(), 1);
    });
}

/// A reverted order Swapic says ran after all (a reorg turned it round) stops its payout for
/// a human, once: nothing is undone, and it is not read again.
#[test]
fn an_order_that_ran_after_its_revert_stops_for_a_human() {
    on_fresh_memory(|| {
        crate::storage::init();
        let id = waiting_withdrawal(staker());
        let owner = PoolAccount::Evm(staker());
        let before = shares_of(&owner);
        sign_payout(id, 0, 9);
        record_at(PoolStep::PayoutReverted { id }, at(10)).unwrap();
        let reads = Reads::ran(true);
        let fake = Fake::default();
        block_on(pass_with(&fake, &reads, || at(11), &open_pool));
        assert!(matches!(
            payout(id).unwrap().status,
            PayoutStatus::Failed(why) if why.starts_with("ALERT")
        ));
        assert_eq!(shares_of(&owner), before);
        block_on(pass_with(&fake, &reads, || at(12), &open_pool));
        assert_eq!(reads.asked.borrow().len(), 1);
    });
}

/// A withdrawal of 100 USDC paid on Base into an Eco intent, its order not one that reads:
/// its fill is read after 900 + an hour (see `fill_read_after`).
fn delivering_withdrawal() -> u64 {
    let id = waiting_withdrawal(staker());
    sign_payout(id, 0, 9);
    record_at(
        PoolStep::PayoutPaid {
            id,
            tx_hash: types::TxHash::new([9; 32]),
            block: BlockNumber::new(30),
            intent: Some([0x1e; 32]),
        },
        at(10),
    )
    .unwrap();
    assert_eq!(payout(id).unwrap().status, PayoutStatus::Delivering);
    id
}

/// Item 7 of the final review fixes: an Eco payout's fill is read once its route can no
/// longer be filled. Read filled, it is paid; read unfilled, it is undone at once (a
/// withdrawal's shares minted back) and its USDC is owed back by Eco; asked too early or not
/// read, it waits for the next pass.
#[test]
fn a_fill_read_filled_delivers_and_unfilled_undoes() {
    let after = || Timestamp::from_secs(900 + 3_600 + FILL_MARGIN_S).unwrap();
    on_fresh_memory(move || {
        crate::storage::init();
        let id = delivering_withdrawal();
        let reads = Reads::fill(Ok(Fill::Filled));
        block_on(pass_with(&Fake::default(), &reads, after, &open_pool));
        assert_eq!(
            *reads.fills.borrow(),
            vec![(ChainId::BASE, [0x1e; 32], 900 + 3_600)]
        );
        assert_eq!(payout(id).unwrap().status, PayoutStatus::Paid);
        assert_eq!(ledger().pockets.base.in_flight, TokenAmount::ZERO);
    });
    on_fresh_memory(move || {
        crate::storage::init();
        let id = delivering_withdrawal();
        let owner = PoolAccount::Evm(staker());
        let burned = match payout(id).unwrap().undo {
            Undo::Remint { shares, .. } => shares,
            Undo::Refund { .. } => unreachable!(),
        };
        let before = shares_of(&owner);
        let amount = payout(id).unwrap().amount.unwrap();
        let reads = Reads::fill(Ok(Fill::Open));
        block_on(pass_with(&Fake::default(), &reads, after, &open_pool));
        let undone = payout(id).unwrap();
        assert_eq!(undone.status, PayoutStatus::Undone(UNFILLED.to_string()));
        assert_eq!(undone.reclaim, Some(types::pool::Reclaim::Pending));
        assert_eq!(shares_of(&owner), before.checked_add(burned).unwrap());
        assert_eq!(ledger().pockets.base.in_flight, amount);
        // read once
        block_on(pass_with(&Fake::default(), &reads, after, &open_pool));
        assert_eq!(reads.fills.borrow().len(), 1);
    });
}

#[test]
fn a_fill_read_too_early_or_unread_waits() {
    on_fresh_memory(|| {
        crate::storage::init();
        let id = delivering_withdrawal();
        // before its route's deadline: not even asked
        let reads = Reads::fill(Ok(Fill::Open));
        block_on(pass_with(
            &Fake::default(),
            &reads,
            || Timestamp::from_secs(900 + 3_600).unwrap(),
            &open_pool,
        ));
        assert!(reads.fills.borrow().is_empty());
        let late = || Timestamp::from_secs(900 + 3_600 + FILL_MARGIN_S).unwrap();
        for answer in [
            crate::agreed::FillRead::TooEarly,
            crate::agreed::FillRead::Unread("no agreement".to_string()),
        ] {
            let reads = Reads::fill(Err(answer));
            block_on(pass_with(&Fake::default(), &reads, late, &open_pool));
            assert_eq!(reads.fills.borrow().len(), 1);
            assert_eq!(payout(id).unwrap().status, PayoutStatus::Delivering);
        }
    });
}

/// An ICP swap paid into an Eco intent that never filled: its user refunded at once, all its
/// ckUSDC less the ledger's fee, with no Swapic fee.
#[test]
fn an_unfilled_icp_swap_refunds_its_ckusdc_at_once() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        record_at(
            PoolStep::PayoutSettingsSet {
                settings: PayoutSettings::default(),
            },
            at(1),
        )
        .unwrap();
        record_at(
            PoolStep::SettingsSet {
                settings: types::pool::PoolSettings {
                    open_for_swaps: true,
                    ledger_fees: vec![types::pool::LedgerFee {
                        ledger: ckusdc(),
                        fee: TokenAmount::from(FEE),
                    }],
                    ..types::pool::PoolSettings::default()
                },
            },
            at(1),
        )
        .unwrap();
        base_stake(staker(), usdc(500), 0x7a, 1);
        let id = ledger().lines().next_id;
        let swap = IcpSwapTerms {
            token_in: ckusdc(),
            amount_in: usdc(100),
            from: account(5),
            min_usdc: usdc(100),
            premium: usdc(1),
            network_fee: TokenAmount::from(50_000_u32),
            refund_to: account(6),
            slippage_bps: None,
            planned: None,
        };
        record_at(
            PoolStep::PayoutOpened {
                id,
                kind: Box::new(PayoutKind::IcpSwap(swap)),
                terms: terms(at(1_000_000_000_000)),
                undo: Undo::Refund { to: account(6) },
                plan: None,
            },
            at(2),
        )
        .unwrap();
        let fake = Fake::default();
        block_on(icp_swap::drive(&fake, id, || at(3), &open_pool)).unwrap();
        assert_eq!(payout(id).unwrap().status, PayoutStatus::Waiting);
        sign_payout(id, 0, 9);
        record_at(
            PoolStep::PayoutPaid {
                id,
                tx_hash: types::TxHash::new([9; 32]),
                block: BlockNumber::new(30),
                intent: Some([0x1f; 32]),
            },
            at(10),
        )
        .unwrap();
        let reads = Reads::fill(Ok(Fill::Cancelled));
        let after = || Timestamp::from_secs(900 + 3_600 + FILL_MARGIN_S).unwrap();
        block_on(pass_with(&fake, &reads, after, &open_pool));
        assert_eq!(
            *fake.transfers.borrow(),
            vec![(
                ckusdc(),
                account(6),
                usdc(100).checked_sub(TokenAmount::from(FEE)).unwrap()
            )]
        );
        let undone = payout(id).unwrap();
        assert_eq!(undone.status, PayoutStatus::Undone(UNFILLED.to_string()));
        assert_eq!(undone.reclaim, Some(types::pool::Reclaim::Pending));
    });
}

/// The lead's check of the deferred Eco accounting: the rebalance split and the Base books
/// leave a delivering or unfilled payout's USDC in flight out, exactly as they leave a signed
/// one's out today.
#[test]
fn the_split_and_the_books_leave_an_eco_payouts_usdc_in_flight_out() {
    on_fresh_memory(|| {
        crate::storage::init();
        let id = waiting_withdrawal(staker());
        sign_payout(id, 0, 9);
        let left_out = || {
            let ledger = ledger();
            let base = ledger.pockets.base;
            let (_, split) = crate::pool::rebalance::trigger::split(&ledger, &[]);
            let held = base.free.checked_add(base.reserved).unwrap();
            assert_eq!(
                split,
                i128::try_from(held.try_into_u128().unwrap()).unwrap()
            );
            assert_eq!(
                ledger.base_held(),
                held.checked_add(ledger.unattributed)
                    .unwrap()
                    .checked_add(ledger.lines().fees_base)
                    .unwrap()
            );
            (split, ledger.base_held(), base.in_flight)
        };
        let signed = left_out();
        assert!(signed.2 > TokenAmount::ZERO);
        record_at(
            PoolStep::PayoutPaid {
                id,
                tx_hash: types::TxHash::new([9; 32]),
                block: BlockNumber::new(30),
                intent: Some([0x1e; 32]),
            },
            at(10),
        )
        .unwrap();
        assert_eq!(left_out(), signed, "delivering");
        record_at(
            PoolStep::PayoutUnfilled {
                id,
                block: BlockNumber::new(31),
            },
            at(11),
        )
        .unwrap();
        // the fee went back to free: in both lines alike; the USDC Eco owes back left out
        assert_eq!(left_out(), signed, "unfilled");
    });
}

/// `resolve_payout` (2026-10-23): an ICP swap stopped for a human (its order ran after its
/// revert, as `used` read it), refunded by a controller: the pass asks its refund at once, all
/// its ckUSDC less the ledger's fee, under the pool's guard, and it is undone once.
#[test]
fn a_stopped_icp_swap_a_controller_refunds_is_refunded_by_the_pass() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        record_at(
            PoolStep::PayoutSettingsSet {
                settings: PayoutSettings::default(),
            },
            at(1),
        )
        .unwrap();
        record_at(
            PoolStep::SettingsSet {
                settings: types::pool::PoolSettings {
                    open_for_swaps: true,
                    ledger_fees: vec![types::pool::LedgerFee {
                        ledger: ckusdc(),
                        fee: TokenAmount::from(FEE),
                    }],
                    ..types::pool::PoolSettings::default()
                },
            },
            at(1),
        )
        .unwrap();
        base_stake(staker(), usdc(500), 0x7b, 1);
        let before = ledger();
        let id = ledger().lines().next_id;
        let swap = IcpSwapTerms {
            token_in: ckusdc(),
            amount_in: usdc(100),
            from: account(5),
            min_usdc: usdc(100),
            premium: usdc(1),
            network_fee: TokenAmount::from(50_000_u32),
            refund_to: account(6),
            slippage_bps: None,
            planned: None,
        };
        record_at(
            PoolStep::PayoutOpened {
                id,
                kind: Box::new(PayoutKind::IcpSwap(swap)),
                terms: terms(at(1_000_000_000_000)),
                undo: Undo::Refund { to: account(6) },
                plan: None,
            },
            at(2),
        )
        .unwrap();
        let fake = Fake::default();
        block_on(icp_swap::drive(&fake, id, || at(3), &open_pool)).unwrap();
        sign_payout(id, 0, 9);
        record_at(PoolStep::PayoutReverted { id }, at(10)).unwrap();
        // the pass reads `used` true: stopped for a human, its lines held
        block_on(pass_with(&fake, &Reads::ran(true), || at(11), &open_pool));
        assert!(matches!(
            payout(id).unwrap().status,
            PayoutStatus::Failed(_)
        ));
        // a pool closed meanwhile pays nothing: the refund waits for the guard
        crate::pool::resolve::resolve(
            false,
            true,
            id,
            settlement_api::types::pool::PayoutResolution::Refund,
            at(12),
        )
        .unwrap();
        let closed = || Err("closed".to_string());
        block_on(pass_with(&fake, &Reads::unread(), || at(13), &closed));
        assert!(fake.transfers.borrow().is_empty());
        block_on(pass_with(&fake, &Reads::unread(), || at(14), &open_pool));
        assert_eq!(
            *fake.transfers.borrow(),
            vec![(
                ckusdc(),
                account(6),
                usdc(100).checked_sub(TokenAmount::from(FEE)).unwrap()
            )]
        );
        assert_eq!(
            payout(id).unwrap().status,
            PayoutStatus::Undone(types::pool::payout::REFUNDED.to_string())
        );
        let after = ledger();
        assert_eq!(after.pockets, before.pockets);
        assert_eq!(after.owed, before.owed);
        assert_eq!(after.lines().fees_icp, TokenAmount::ZERO);
        // once
        block_on(pass_with(&fake, &Reads::unread(), || at(15), &open_pool));
        assert_eq!(fake.transfers.borrow().len(), 1);
    });
}

/// A withdrawal whose undo began (a controller's refund whose remint did not land in its own
/// call) is minted back by the pass: no ledger is asked.
#[test]
fn the_pass_finishes_a_withdrawals_undo() {
    let mut withdrawal = payout_in(PayoutStatus::Undoing("refunded".to_string()), secs(10_000));
    withdrawal.kind = PayoutKind::Withdrawal(types::pool::EvmWithdrawal {
        staker: staker(),
        usdc: usdc(10),
        max_shares: types::pool::Shares::new(1),
        rebalance_share: TokenAmount::ZERO,
        network_fee: TokenAmount::ZERO,
        nonce: 1,
        burned: types::pool::Shares::new(1),
    });
    assert_eq!(
        next(&withdrawal, &PayoutSettings::default(), secs(1), false),
        Next::Undo {
            why: "refunded".to_string(),
            checked: false
        }
    );
}

/// The review (2026-10-23): a planned payout whose plan waited is tried again by the pass
/// while it can still fit (within the window, its order's deadline ahead) and has tries left;
/// otherwise it waits for the watcher's order as before.
#[test]
fn a_planned_payout_whose_plan_waited_is_tried_again_while_it_can_fit() {
    let settings = PayoutSettings::default();
    let mut planned = payout_in(PayoutStatus::Waiting, secs(10_000));
    if let PayoutKind::IcpSwap(terms) = &mut planned.kind {
        terms.planned = Some(usdc(100));
        terms.slippage_bps = Some(100);
    }
    planned.amount = Some(usdc(101));
    planned.plan = Some(PinnedOrder {
        order_id: [3; 32],
        order: vec![3],
        gas_limit: 300_000,
        deadline_s: 2_000,
    });
    planned.fit_tries = Some(1);
    let decide = |payout: &EvmPayout, now| next(payout, &settings, now, false);
    assert_eq!(decide(&planned, secs(60)), Next::Fit);
    // its tries spent
    let mut spent = planned.clone();
    spent.fit_tries = Some(types::pool::payout::MAX_FIT_TRIES);
    assert_eq!(decide(&spent, secs(60)), Next::Wait);
    // outside the window
    let mut far = planned.clone();
    far.amount = Some(usdc(106));
    assert_eq!(decide(&far, secs(60)), Next::Wait);
    // its order's deadline under a minute away
    assert_eq!(decide(&planned, secs(1_941)), Next::Relist);
    // signed once: never tried again
    let mut signed = planned.clone();
    signed.order = Some(planned.plan.clone().unwrap());
    assert_eq!(decide(&signed, secs(60)), Next::Wait);
}
