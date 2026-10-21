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
use types::{ChainId, TokenAmount};

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
    // reverted, its order pinned: signed again while the order's deadline is ahead; undone
    // past the payout's only once Swapic says it never ran
    let mut reverted = payout_in(PayoutStatus::Waiting, deadline);
    reverted.order = Some(PinnedOrder {
        order_id: [1; 32],
        order: vec![1],
        gas_limit: 300_000,
        deadline_s: 2_000,
    });
    assert_eq!(decide(&reverted, before, false), Next::Resign);
    assert_eq!(decide(&reverted, secs(1_941), false), Next::Wait);
    assert!(matches!(
        decide(&reverted, past, false),
        Next::Undo { checked: true, .. }
    ));
    // a transaction still being signed or in the outbox: nothing
    assert_eq!(decide(&reverted, before, true), Next::Wait);
    assert_eq!(decide(&reverted, past, true), Next::Wait);
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
        };
        record_at(
            PoolStep::PayoutOpened {
                id,
                kind: Box::new(PayoutKind::IcpSwap(swap)),
                terms: terms(deadline),
                undo: Undo::Refund { to: account(6) },
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
        // reverted with its order pinned, the revert not yet closed for good: ran() cannot be
        // read off the canister, so nothing is undone
        record_at(PoolStep::PayoutReverted { id }, at(10)).unwrap();
        block_on(pass(&fake, || deadline, &open_pool));
        assert_eq!(payout(id).unwrap().status, PayoutStatus::Waiting);
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
        block_on(pass(&fake, || deadline, &open_pool));
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
