use super::*;
use crate::storage::on_fresh_memory;
use crate::storage::pool::tests::{
    account, at, ckusdc, credit, funded_pool, register, settings, swap_terms, usdc, FEE,
};
use crate::storage::pool::{deposit, ledger, record_at};
use std::cell::Cell;
use std::collections::BTreeMap;
use types::pool::{IcpToken, LedgerFee, PoolAccount, Shares};
use types::BlockNumber;

/// Runs a future that never waits: every call of the fake ledger answers at once.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    match future.as_mut().poll(&mut context) {
        std::task::Poll::Ready(out) => out,
        std::task::Poll::Pending => panic!("the fake ledger never waits"),
    }
}

fn clock() -> Timestamp {
    at(10)
}

fn token() -> Principal {
    Principal::from_slice(&[0x70; 10])
}

fn icpswap_pool() -> Principal {
    Principal::from_slice(&[0x71; 10])
}

const TOKEN_FEE: u64 = 1_000;

/// A transfer's ledger and dedup pair (memo, `created_at_time`).
type TransferKey = (Principal, [u8; 32], u64);
/// Where it went, how much, and the block it landed in.
type Executed = (IcpAccount, TokenAmount, u64);

/// A ledger that executes a transfer once per dedup pair, as an ICRC ledger does, and
/// remembers what each call saw of the fold when it was made.
#[derive(Default)]
struct Fake {
    calls: RefCell<Vec<String>>,
    /// (ledger, memo, created_at) to (to, amount, block)
    executed: RefCell<BTreeMap<TransferKey, Executed>>,
    lose_answers: Cell<u32>,
    quote: Cell<Option<TokenAmount>>,
    /// None: the swap's answer never comes
    swap_out: Cell<Option<TokenAmount>>,
    /// The status of the deposit under watch at each money call
    watch: Cell<Option<PoolRef>>,
    seen: RefCell<Vec<Option<DepositStatus>>>,
}

impl Fake {
    fn saw(&self) {
        if let Some(r#ref) = self.watch.get() {
            self.seen
                .borrow_mut()
                .push(deposit(&r#ref).map(|deposit| deposit.status));
        }
    }

    fn transfers(&self) -> Vec<(Principal, IcpAccount, TokenAmount)> {
        self.executed
            .borrow()
            .iter()
            .map(|((ledger, _, _), (to, amount, _))| (*ledger, *to, *amount))
            .collect()
    }
}

impl Ledgers for Fake {
    async fn transfer(
        &self,
        ledger: Principal,
        to: IcpAccount,
        amount: TokenAmount,
        dedup: Dedup,
    ) -> Result<u64, CallError> {
        self.saw();
        self.calls.borrow_mut().push(format!("transfer {amount}"));
        let key = (ledger, dedup.memo, dedup.created_at_nanos);
        let mut executed = self.executed.borrow_mut();
        // the ledger's deduplication: the same transfer answers the block it landed in
        if let Some((_, _, block)) = executed.get(&key) {
            return Ok(*block);
        }
        let block = executed.len() as u64 + 100;
        executed.insert(key, (to, amount, block));
        if self.lose_answers.get() > 0 {
            self.lose_answers.set(self.lose_answers.get() - 1);
            return Err(CallError::Unknown("lost".into()));
        }
        Ok(block)
    }

    async fn pull(
        &self,
        _ledger: Principal,
        _from: IcpAccount,
        amount: TokenAmount,
        _dedup: Dedup,
    ) -> Result<u64, CallError> {
        self.calls.borrow_mut().push(format!("pull {amount}"));
        Ok(7)
    }

    async fn approve(
        &self,
        _ledger: Principal,
        _spender: Principal,
        amount: TokenAmount,
    ) -> Result<(), CallError> {
        self.saw();
        self.calls.borrow_mut().push(format!("approve {amount}"));
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
        amount_in: TokenAmount,
        _zero_for_one: bool,
    ) -> Result<TokenAmount, CallError> {
        self.calls.borrow_mut().push(format!("quote {amount_in}"));
        Ok(self.quote.get().unwrap_or(TokenAmount::ZERO))
    }

    async fn swap(
        &self,
        _pool: Principal,
        amount_in: TokenAmount,
        _zero_for_one: bool,
        min_out: TokenAmount,
        _fee_in: TokenAmount,
        _fee_out: TokenAmount,
    ) -> Result<TokenAmount, CallError> {
        self.saw();
        self.calls
            .borrow_mut()
            .push(format!("swap {amount_in} at least {min_out}"));
        self.swap_out
            .get()
            .ok_or_else(|| CallError::Unknown("the pool did not answer".into()))
    }
}

/// A pool with 1000 ckUSDC on ICP, a listed token, and a ckUSDC swap of 100 USDC for
/// `account(1)` arrived.
fn arrived_swap(token_out: Principal, min_out: u128) -> PoolRef {
    funded_pool(usdc(1_000));
    record_at(
        PoolStep::SettingsSet {
            settings: PoolSettings {
                icp_tokens: vec![IcpToken {
                    ledger: token(),
                    icpswap_pool: icpswap_pool(),
                }],
                ledger_fees: vec![
                    LedgerFee {
                        ledger: ckusdc(),
                        fee: TokenAmount::from(FEE),
                    },
                    LedgerFee {
                        ledger: token(),
                        fee: TokenAmount::from(TOKEN_FEE),
                    },
                ],
                ..settings()
            },
        },
        at(1),
    )
    .unwrap();
    let mut terms = swap_terms(1, usdc(100), usdc(1), at(1_000));
    terms.kind = DepositKind::Swap {
        recipient: account(1),
        token_out,
        min_out,
    };
    let r#ref = register(terms);
    record_at(
        PoolStep::Arrived {
            r#ref,
            credit: credit(1),
            amount: usdc(100),
            block: BlockNumber::new(5),
        },
        at(3),
    )
    .unwrap();
    r#ref
}

/// Review Focus 2's first half: `PayoutStarted` with its dedup pair is in the log before
/// any call, and the recipient gets exactly `arrived - premium`.
#[test]
fn the_payout_starts_before_any_call_and_pays_arrived_less_premium() {
    on_fresh_memory(|| {
        crate::storage::init();
        let r#ref = arrived_swap(ckusdc(), 1);
        let fake = Fake::default();
        fake.watch.set(Some(r#ref));
        block_on(drive_deposit(&fake, r#ref, clock)).unwrap();
        assert_eq!(*fake.seen.borrow(), vec![Some(DepositStatus::Paying)]);
        assert_eq!(fake.transfers(), vec![(ckusdc(), account(1), usdc(99))]);
        let paid = deposit(&r#ref).unwrap();
        assert_eq!(paid.status, DepositStatus::Paid);
        let attempt = paid.payout.attempt.unwrap();
        assert_eq!(attempt.memo, r#ref.into_bytes());
        assert_eq!(attempt.created_at, clock());
        assert_eq!(
            paid.paid.unwrap().spent,
            usdc(99).checked_add(TokenAmount::from(FEE)).unwrap()
        );
        let pool = ledger();
        assert_eq!(pool.owed, TokenAmount::ZERO);
        assert_eq!(pool.pockets.icp.reserved, TokenAmount::ZERO);
        assert!(pool.paying.is_empty());
    });
}

/// Review Focus 2: a transfer whose answer was lost is asked again with the same
/// `created_at_time` and memo; the ledger answers the earlier transfer, and the recipient
/// is paid once.
#[test]
fn a_lost_answer_is_asked_again_and_paid_once() {
    on_fresh_memory(|| {
        crate::storage::init();
        let r#ref = arrived_swap(ckusdc(), 1);
        let fake = Fake::default();
        fake.lose_answers.set(1);
        let stop = block_on(drive_deposit(&fake, r#ref, clock)).unwrap_err();
        assert!(matches!(stop, Stop::Failed(_)));
        fail(r#ref, "lost".into(), clock());
        assert_eq!(deposit(&r#ref).unwrap().status, DepositStatus::Paying);
        // the next drive, later: the same pair, so the same transfer
        block_on(drive_deposit(&fake, r#ref, || at(99))).unwrap();
        assert_eq!(fake.transfers(), vec![(ckusdc(), account(1), usdc(99))]);
        assert_eq!(fake.calls.borrow().len(), 2, "asked twice, executed once");
        let paid = deposit(&r#ref).unwrap();
        assert_eq!(paid.status, DepositStatus::Paid);
        assert_eq!(paid.paid.unwrap().block, 100);
    });
}

#[test]
fn a_stake_mints_its_shares_at_the_price_before_it() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        let owner = PoolAccount::Icp(account(5));
        let terms = types::pool::Registration {
            kind: DepositKind::Stake { owner },
            premium: TokenAmount::ZERO,
            rebalance_share: TokenAmount::from(500_000_u32),
            ..swap_terms(5, usdc(100), TokenAmount::ZERO, at(1_000))
        };
        let r#ref = register(terms);
        record_at(
            PoolStep::Arrived {
                r#ref,
                credit: credit(1),
                amount: usdc(100),
                block: BlockNumber::new(5),
            },
            at(3),
        )
        .unwrap();
        let expected = Shares::mint_for(
            TokenAmount::from(99_500_000_u32),
            usdc(1_000),
            ledger().total_shares,
        )
        .unwrap();
        let fake = Fake::default();
        block_on(drive_deposit(&fake, r#ref, clock)).unwrap();
        assert!(fake.calls.borrow().is_empty(), "a stake calls no ledger");
        assert_eq!(deposit(&r#ref).unwrap().minted, Some(expected));
        assert_eq!(crate::storage::pool::shares_of(&owner), expected);
    });
}

/// Review Focus 5: a quote below `min_out` is tried once more, then ALL of it is paid as
/// ckUSDC with the same dedup pair: never a mix, never nothing, nothing approved or swapped.
#[test]
fn a_leg_below_its_minimum_pays_all_of_it_as_ckusdc() {
    on_fresh_memory(|| {
        crate::storage::init();
        let r#ref = arrived_swap(token(), 5_000);
        let fake = Fake::default();
        // the recipient needs 5000 after the token's 1000 fee: 5999 is one short
        fake.quote.set(Some(TokenAmount::from(5_999_u32)));
        assert_eq!(
            block_on(drive_deposit(&fake, r#ref, clock)),
            Err(Stop::Later)
        );
        assert_eq!(deposit(&r#ref).unwrap().payout.swap.declined, 1);
        assert!(fake.transfers().is_empty());
        block_on(drive_deposit(&fake, r#ref, clock)).unwrap();
        assert_eq!(fake.transfers(), vec![(ckusdc(), account(1), usdc(99))]);
        assert!(!fake
            .calls
            .borrow()
            .iter()
            .any(|call| call.starts_with("approve") || call.starts_with("swap")));
        let paid = deposit(&r#ref).unwrap();
        assert_eq!(paid.status, DepositStatus::Paid);
        assert_eq!(paid.paid.unwrap().token, ckusdc());
        assert!(paid.payout.fell_back.is_some());
    });
}

/// The ICPSwap leg: quote, approve, `SwapBegun`, the swap at the minimum plus the token's
/// fee, `Swapped`, and the output less its fee to the recipient.
#[test]
fn an_icpswap_leg_pays_its_output() {
    on_fresh_memory(|| {
        crate::storage::init();
        let r#ref = arrived_swap(token(), 5_000);
        let fake = Fake::default();
        fake.watch.set(Some(r#ref));
        fake.quote.set(Some(TokenAmount::from(8_000_u32)));
        fake.swap_out.set(Some(TokenAmount::from(7_900_u32)));
        block_on(drive_deposit(&fake, r#ref, clock)).unwrap();
        assert_eq!(
            *fake.calls.borrow(),
            vec![
                format!("quote {}", usdc(99)),
                format!(
                    "approve {}",
                    usdc(99).checked_add(TokenAmount::from(FEE)).unwrap()
                ),
                format!("swap {} at least 6000", usdc(99)),
                "transfer 6900".to_string(),
            ]
        );
        assert_eq!(
            fake.transfers(),
            vec![(token(), account(1), TokenAmount::from(6_900_u32))]
        );
        let paid = deposit(&r#ref).unwrap();
        assert_eq!(paid.status, DepositStatus::Paid);
        assert_eq!(
            paid.paid.unwrap().spent,
            usdc(99).checked_add(TokenAmount::from(2 * FEE)).unwrap()
        );
    });
}

/// A swap whose answer never came is never called again: its outcome is unknown, and the
/// payout fails until it stops for a human.
#[test]
fn a_swap_with_no_answer_is_never_called_again() {
    on_fresh_memory(|| {
        crate::storage::init();
        let r#ref = arrived_swap(token(), 5_000);
        let fake = Fake::default();
        fake.quote.set(Some(TokenAmount::from(8_000_u32)));
        let stop = block_on(drive_deposit(&fake, r#ref, clock)).unwrap_err();
        assert!(matches!(stop, Stop::Failed(_)));
        assert!(deposit(&r#ref).unwrap().payout.swap.begun);
        let swaps = |fake: &Fake| {
            fake.calls
                .borrow()
                .iter()
                .filter(|call| call.starts_with("swap"))
                .count()
        };
        assert_eq!(swaps(&fake), 1);
        let stop = block_on(drive_deposit(&fake, r#ref, clock)).unwrap_err();
        assert_eq!(
            stop,
            Stop::Failed("the ICPSwap swap's outcome is unknown".to_string())
        );
        assert_eq!(swaps(&fake), 1);
        assert!(fake.transfers().is_empty());
        for _ in 0..types::pool::MAX_PAYOUT_ATTEMPTS {
            fail(
                r#ref,
                "the ICPSwap swap's outcome is unknown".into(),
                clock(),
            );
        }
        assert!(matches!(
            deposit(&r#ref).unwrap().status,
            DepositStatus::PayoutFailed(_)
        ));
        assert!(ledger().paying.is_empty());
    });
}
