use super::*;
use crate::storage::on_fresh_memory;
use crate::storage::pool::tests::{
    account, at, ckusdc, credit, funded_pool, register, settings, swap_terms, usdc, FEE,
};
use crate::storage::pool::{deposit, ledger, record_at};
use std::cell::Cell;
use std::collections::BTreeMap;
use types::hash::PoolOpId;
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

fn open_pool() -> Result<(), String> {
    Ok(())
}

fn closed_pool() -> Result<(), String> {
    Err("a controller paused the pool".to_string())
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
    /// The ledger refuses the next transfer: nothing moves.
    refuse: Cell<bool>,
    /// What the next pull answers instead of landing.
    pull_answer: RefCell<Option<CallError>>,
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
        if self.refuse.replace(false) {
            return Err(CallError::Refused("InsufficientFunds".into()));
        }
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
        match self.pull_answer.borrow_mut().take() {
            Some(error) => Err(error),
            None => Ok(7),
        }
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
        block_on(drive_deposit(&fake, r#ref, clock, &open_pool)).unwrap();
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
        let stop = block_on(drive_deposit(&fake, r#ref, clock, &open_pool)).unwrap_err();
        // an outcome unknown: nothing recorded, no attempt counted, asked again as it is
        assert!(matches!(stop, Stop::Retry(_)));
        let waiting = deposit(&r#ref).unwrap();
        assert_eq!(waiting.status, DepositStatus::Paying);
        assert_eq!(waiting.payout.failures, 0);
        // the next drive, later: the same pair, so the same transfer
        block_on(drive_deposit(&fake, r#ref, || at(99), &open_pool)).unwrap();
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
        block_on(drive_deposit(&fake, r#ref, clock, &open_pool)).unwrap();
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
            block_on(drive_deposit(&fake, r#ref, clock, &open_pool)),
            Err(Stop::Later)
        );
        assert_eq!(deposit(&r#ref).unwrap().payout.swap.declined, 1);
        assert!(fake.transfers().is_empty());
        block_on(drive_deposit(&fake, r#ref, clock, &open_pool)).unwrap();
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
        block_on(drive_deposit(&fake, r#ref, clock, &open_pool)).unwrap();
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
        let stop = block_on(drive_deposit(&fake, r#ref, clock, &open_pool)).unwrap_err();
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
        let stop = block_on(drive_deposit(&fake, r#ref, clock, &open_pool)).unwrap_err();
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
        // stopped, and still between its start and its payment for the ICP check
        assert!(ledger().paying.contains(&r#ref));
    });
}

/// Review fix: the pool's guard is read before every outflow call: a pool closed after the
/// payout started pays nothing, records nothing more, and counts no attempt.
#[test]
fn a_pool_closed_before_an_outflow_pays_nothing() {
    on_fresh_memory(|| {
        crate::storage::init();
        let r#ref = arrived_swap(ckusdc(), 1);
        let fake = Fake::default();
        let stop = block_on(drive_deposit(&fake, r#ref, clock, &closed_pool)).unwrap_err();
        assert!(matches!(stop, Stop::Retry(why) if why.contains("closed")));
        assert!(fake.calls.borrow().is_empty(), "no ledger was called");
        let waiting = deposit(&r#ref).unwrap();
        assert_eq!(waiting.status, DepositStatus::Paying);
        assert_eq!(waiting.payout.failures, 0);
        // reopened: paid, once
        block_on(drive_deposit(&fake, r#ref, clock, &open_pool)).unwrap();
        assert_eq!(fake.transfers(), vec![(ckusdc(), account(1), usdc(99))]);

        // an ICPSwap leg whose pool closes after the quote: no approval, no swap
        let r#ref = {
            let mut terms = swap_terms(2, usdc(100), usdc(1), at(1_000));
            terms.kind = DepositKind::Swap {
                recipient: account(2),
                token_out: token(),
                min_out: 1,
            };
            let r#ref = register(terms);
            record_at(
                PoolStep::Arrived {
                    r#ref,
                    credit: credit(2),
                    amount: usdc(100),
                    block: BlockNumber::new(6),
                },
                at(3),
            )
            .unwrap();
            r#ref
        };
        let fake = Fake::default();
        fake.quote.set(Some(TokenAmount::from(8_000_u32)));
        let stop = block_on(drive_deposit(&fake, r#ref, clock, &closed_pool)).unwrap_err();
        assert!(matches!(stop, Stop::Retry(_)));
        assert!(!fake
            .calls
            .borrow()
            .iter()
            .any(|call| call.starts_with("approve") || call.starts_with("swap")));
        assert!(!deposit(&r#ref).unwrap().payout.swap.begun);
    });
}

/// A refusal is definite: nothing moved, the attempt counts, and the next drive asks again.
#[test]
fn a_refused_transfer_counts_an_attempt() {
    on_fresh_memory(|| {
        crate::storage::init();
        let r#ref = arrived_swap(ckusdc(), 1);
        let fake = Fake::default();
        fake.refuse.set(true);
        let stop = block_on(drive_deposit(&fake, r#ref, clock, &open_pool)).unwrap_err();
        let Stop::Failed(why) = stop else {
            panic!("a refusal counts: {stop:?}")
        };
        fail(r#ref, why, clock());
        assert_eq!(deposit(&r#ref).unwrap().payout.failures, 1);
        assert!(fake.transfers().is_empty());
        block_on(drive_deposit(&fake, r#ref, clock, &open_pool)).unwrap();
        assert_eq!(deposit(&r#ref).unwrap().status, DepositStatus::Paid);
    });
}

/// A stake's pull: refused, nothing moved and the room goes back; unknown, asked again; too
/// old for the ledger to tell, never recorded as failed (it may have run): it waits for a
/// human, and the drive stops asking once the ledger can no longer deduplicate it.
#[test]
fn a_stakes_pull_is_failed_only_when_nothing_moved() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        let started = |byte: u8| {
            let id = PoolOpId::new([byte; 32]);
            record_at(
                PoolStep::StakeStarted {
                    id,
                    owner: PoolAccount::Icp(account(byte)),
                    from: account(byte),
                    amount: usdc(10),
                    created_at: at(2),
                },
                at(2),
            )
            .unwrap();
            id
        };
        let fake = Fake::default();
        let refused = started(1);
        *fake.pull_answer.borrow_mut() = Some(CallError::Refused("InsufficientAllowance".into()));
        assert!(matches!(
            block_on(pull_stake(&fake, refused, clock)),
            Err(Stop::Failed(_))
        ));
        assert!(!ledger().pending.contains_key(&refused));
        assert_eq!(ledger().stake_room, TokenAmount::ZERO);

        let unknown = started(2);
        *fake.pull_answer.borrow_mut() = Some(CallError::Unknown("rejected".into()));
        assert!(matches!(
            block_on(pull_stake(&fake, unknown, clock)),
            Err(Stop::Retry(_))
        ));
        assert!(ledger().pending.contains_key(&unknown));
        // asked again, it lands: minted
        assert!(block_on(pull_stake(&fake, unknown, clock))
            .unwrap()
            .is_some());
        assert!(!ledger().pending.contains_key(&unknown));

        let too_old = started(3);
        *fake.pull_answer.borrow_mut() = Some(CallError::Expired("TooOld".into()));
        assert!(matches!(
            block_on(pull_stake(&fake, too_old, clock)),
            Err(Stop::Failed(why)) if why.starts_with("ALERT")
        ));
        assert!(
            ledger().pending.contains_key(&too_old),
            "never recorded as failed"
        );
        assert_eq!(ledger().stake_room, usdc(10));
        // past the ledger's window the drive no longer asks
        let calls = fake.calls.borrow().len();
        let a_day_later = || at(2 + DEDUP_WINDOW.as_nanos() as u64 + 1);
        block_on(drive_with(&fake, a_day_later, &open_pool));
        assert_eq!(fake.calls.borrow().len(), calls);
    });
}

/// Jeff's hold rule at the drive: an arrived swap the ICP pocket's free ckUSDC cannot cover
/// is queued, and the one behind it does not start ahead of it; once the pocket covers them
/// they start first in first out, each dedup instant taken when it really starts.
#[test]
fn queued_payouts_start_first_in_first_out_once_covered() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(150));
        let arrive = |byte: u8, amount: TokenAmount| {
            let r#ref = register(swap_terms(byte, usdc(10), usdc(1), at(1_000)));
            record_at(
                PoolStep::Arrived {
                    r#ref,
                    credit: credit(byte.into()),
                    amount,
                    block: BlockNumber::new(5),
                },
                at(3),
            )
            .unwrap();
            r#ref
        };
        let big = arrive(1, usdc(200));
        let small = arrive(2, usdc(10));
        let fake = Fake::default();
        block_on(drive_with(&fake, clock, &open_pool));
        assert!(
            fake.transfers().is_empty(),
            "the head does not fit, nothing jumps it"
        );
        assert_eq!(deposit(&big).unwrap().status, DepositStatus::Arrived);
        assert_eq!(deposit(&small).unwrap().status, DepositStatus::Arrived);
        // the pocket grows by a stake on ICP
        let id = PoolOpId::new([0x55; 32]);
        record_at(
            PoolStep::StakeStarted {
                id,
                owner: PoolAccount::Icp(account(9)),
                from: account(9),
                amount: usdc(100),
                created_at: at(4),
            },
            at(4),
        )
        .unwrap();
        let shares = ledger().mint_now(usdc(100)).unwrap();
        record_at(
            PoolStep::StakedIcp {
                id,
                owner: PoolAccount::Icp(account(9)),
                amount: usdc(100),
                rebalance_share: TokenAmount::ZERO,
                shares,
                block: 2,
            },
            at(4),
        )
        .unwrap();
        let later = || at(77);
        block_on(drive_with(&fake, later, &open_pool));
        // in the queue's order
        assert_eq!(
            *fake.calls.borrow(),
            vec![
                format!("transfer {}", usdc(199)),
                format!("transfer {}", usdc(9))
            ]
        );
        let mut paid = fake.transfers();
        paid.sort_by_key(|(_, to, _)| to.owner);
        assert_eq!(
            paid,
            vec![
                (ckusdc(), account(1), usdc(199)),
                (ckusdc(), account(2), usdc(9)),
            ]
        );
        for r#ref in [big, small] {
            let paid = deposit(&r#ref).unwrap();
            assert_eq!(paid.status, DepositStatus::Paid);
            assert_eq!(paid.payout.attempt.unwrap().created_at, at(77));
        }
        assert!(ledger().queue.0.is_empty());
        assert_eq!(ledger().pockets.icp.reserved, TokenAmount::ZERO);
    });
}
