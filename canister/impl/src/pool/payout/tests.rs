use super::*;
use crate::storage::on_fresh_memory;
use crate::storage::pool::tests::{
    account, at, ckusdc, credit, funded_pool, register, settings, swap_terms, usdc, FEE,
};
use crate::storage::pool::{deposit, ledger, record_at};
use std::cell::Cell;
use std::collections::BTreeMap;
use types::hash::PoolOpId;
use types::pool::{IcpToken, LedgerFee, PoolAccount, PoolHeld, PrincipalBytes, Shares};
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
    /// The pool answers the swap with its own `err`.
    swap_refused: Cell<bool>,
    /// The canister's unused balances at the pool, token0 (ckUSDC) and token1; None: none.
    unused: Cell<Option<(TokenAmount, TokenAmount)>>,
    /// What the next swap leaves of the canister's at the pool, ckUSDC and token, added to
    /// its unused balances: its deposit when the swap did not happen, its output when the
    /// output was not withdrawn.
    swap_leaves: Cell<Option<(TokenAmount, TokenAmount)>>,
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
        if let Some((ck, token)) = self.swap_leaves.take() {
            let (held_ck, held_token) = self
                .unused
                .get()
                .unwrap_or((TokenAmount::ZERO, TokenAmount::ZERO));
            self.unused.set(Some((
                held_ck.checked_add(ck).unwrap(),
                held_token.checked_add(token).unwrap(),
            )));
        }
        if self.swap_refused.replace(false) {
            return Err(CallError::Refused("the pool answered Err(slippage)".into()));
        }
        self.swap_out
            .get()
            .ok_or_else(|| CallError::Unknown("the pool did not answer".into()))
    }

    async fn unused(&self, _pool: Principal) -> Result<(TokenAmount, TokenAmount), CallError> {
        self.calls.borrow_mut().push("unused".to_string());
        Ok(self
            .unused
            .get()
            .unwrap_or((TokenAmount::ZERO, TokenAmount::ZERO)))
    }

    async fn withdraw(
        &self,
        _pool: Principal,
        token: Principal,
        amount: TokenAmount,
        fee: TokenAmount,
    ) -> Result<(), CallError> {
        let name = if token == ckusdc() { "ckUSDC" } else { "token" };
        self.calls
            .borrow_mut()
            .push(format!("withdraw {name} {amount} fee {fee}"));
        let (ck, other) = self
            .unused
            .get()
            .unwrap_or((TokenAmount::ZERO, TokenAmount::ZERO));
        let less = |left: TokenAmount| left.checked_sub(amount).unwrap_or(TokenAmount::ZERO);
        self.unused.set(Some(if token == ckusdc() {
            (less(ck), other)
        } else {
            (ck, less(other))
        }));
        Ok(())
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
        // the transfer's fee is the user's (P11d)
        assert_eq!(fake.transfers(), vec![(ckusdc(), account(1), less(99, 1))]);
        let paid = deposit(&r#ref).unwrap();
        assert_eq!(paid.status, DepositStatus::Paid);
        let attempt = paid.payout.attempt.unwrap();
        assert_eq!(attempt.memo, r#ref.into_bytes());
        assert_eq!(attempt.created_at, clock());
        assert_eq!(paid.paid.unwrap().spent, usdc(99));
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
        assert_eq!(fake.transfers(), vec![(ckusdc(), account(1), less(99, 1))]);
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
            icp_fees: None,
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
        // the recipient needs 5000 after the token's 1000 fee, twice (the pool's withdrawal
        // to the canister and the canister's transfer): 6999 is one short
        fake.quote.set(Some(TokenAmount::from(6_999_u32)));
        assert_eq!(
            block_on(drive_deposit(&fake, r#ref, clock, &open_pool)),
            Err(Stop::Later)
        );
        assert_eq!(deposit(&r#ref).unwrap().payout.swap.declined, 1);
        assert!(fake.transfers().is_empty());
        block_on(drive_deposit(&fake, r#ref, clock, &open_pool)).unwrap();
        assert_eq!(fake.transfers(), vec![(ckusdc(), account(1), less(99, 1))]);
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
/// fee twice, `Swapped`, and the output less those fees to the recipient.
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
                // the approval's and the pull's fees out of the user's 99
                format!("quote {}", less(99, 2)),
                // what the pool holds of the canister's before the swap
                "unused".to_string(),
                format!("approve {}", less(99, 1)),
                format!("swap {} at least 7000", less(99, 2)),
                "transfer 5900".to_string(),
            ]
        );
        assert_eq!(
            fake.transfers(),
            vec![(token(), account(1), TokenAmount::from(5_900_u32))]
        );
        let leg = deposit(&r#ref).unwrap().payout.swap;
        assert_eq!(leg.approvals, Some(1));
        assert_eq!(leg.send, Some(TokenAmount::from(5_900_u32)));
        let paid = deposit(&r#ref).unwrap();
        assert_eq!(paid.status, DepositStatus::Paid);
        assert_eq!(paid.paid.unwrap().spent, usdc(99));
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
        assert_eq!(fake.transfers(), vec![(ckusdc(), account(1), less(99, 1))]);

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
                format!("transfer {}", less(199, 1)),
                format!("transfer {}", less(9, 1))
            ]
        );
        let mut paid = fake.transfers();
        paid.sort_by_key(|(_, to, _)| to.owner);
        assert_eq!(
            paid,
            vec![
                (ckusdc(), account(1), less(199, 1)),
                (ckusdc(), account(2), less(9, 1)),
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

/// Review fix (head of line, and fix 10): a queued head the ICP pocket cannot cover holds
/// back only the payouts behind it. With the pool closed, what moves nothing out still runs
/// (a stake's pull asked again, an arrived stake's mint); with it open, the payout already
/// started and the withdrawal are paid around the head, and the head, once it has waited a
/// books interval, is alerted on.
#[test]
fn a_waiting_head_holds_back_only_what_queues_behind_it() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(400));
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
        // a payout already started, holding 49 (its fee the user's)
        let started = arrive(1, usdc(50));
        record_at(
            PoolStep::PayoutStarted {
                r#ref: started,
                attempt: PayoutAttempt {
                    created_at: at(4),
                    memo: started.into_bytes(),
                    path: PayoutPath::Ckusdc,
                    amount: None,
                    token: None,
                },
            },
            at(4),
        )
        .unwrap();
        // a withdrawal of 100 by the staker, reserved
        let owner = account(200);
        let id = PoolOpId::new([0x61; 32]);
        let ledger_now = ledger();
        let burned = Shares::burn_for(
            usdc(100),
            ledger_now.total_assets(),
            ledger_now.total_shares,
        )
        .unwrap();
        record_at(
            PoolStep::WithdrawStarted {
                id,
                owner,
                burned,
                usdc: usdc(100),
                to: owner,
                token_out: ckusdc(),
                min_out: 0,
                attempt: PayoutAttempt {
                    created_at: at(4),
                    memo: id.into_bytes(),
                    path: PayoutPath::Ckusdc,
                    amount: None,
                    token: None,
                },
            },
            at(4),
        )
        .unwrap();
        // the head: 300 arrives, its 299 more than the 251 free
        let head = arrive(2, usdc(300));
        assert!(ledger().queue_need().is_some());
        // an arrived EVM stake to mint, and an ICP stake's pull to ask again
        let staked = register(types::pool::Registration {
            kind: DepositKind::Stake {
                owner: PoolAccount::Icp(account(3)),
            },
            premium: TokenAmount::ZERO,
            icp_fees: None,
            ..swap_terms(3, usdc(10), TokenAmount::ZERO, at(1_000))
        });
        record_at(
            PoolStep::Arrived {
                r#ref: staked,
                credit: credit(3),
                amount: usdc(10),
                block: BlockNumber::new(5),
            },
            at(3),
        )
        .unwrap();
        let pull = PoolOpId::new([0x62; 32]);
        record_at(
            PoolStep::StakeStarted {
                id: pull,
                owner: PoolAccount::Icp(account(4)),
                from: account(4),
                amount: usdc(1),
                created_at: at(4),
            },
            at(4),
        )
        .unwrap();

        let fake = Fake::default();
        block_on(drive_with(&fake, clock, &closed_pool));
        assert_eq!(
            *fake.calls.borrow(),
            vec![format!("pull {}", usdc(1))],
            "closed: the pull is asked again, nothing goes out"
        );
        assert!(
            deposit(&staked).unwrap().minted.is_some(),
            "and the stake is minted"
        );

        let a_while_later = || at(4 + 601_000_000_000);
        block_on(drive_with(&fake, a_while_later, &open_pool));
        assert_eq!(deposit(&started).unwrap().status, DepositStatus::Paid);
        assert!(ledger().pending.is_empty(), "the withdrawal is paid");
        assert_eq!(deposit(&head).unwrap().status, DepositStatus::Arrived);
        assert_eq!(ledger().queue.head().map(|(r#ref, _)| r#ref), Some(head));
        assert_eq!(
            crate::storage::timer_state::get().alerted_head,
            Some(head),
            "the alert on the head is kept in stable memory"
        );
    });
}

/// Another swap of 100 USDC for `account(byte)`, paid in `token_out`, arrived in a pool
/// `arrived_swap` set up.
fn arrive(byte: u8, token_out: Principal, min_out: u128) -> PoolRef {
    let mut terms = swap_terms(byte, usdc(100), usdc(1), at(1_000));
    terms.kind = DepositKind::Swap {
        recipient: account(byte),
        token_out,
        min_out,
    };
    let r#ref = register(terms);
    record_at(
        PoolStep::Arrived {
            r#ref,
            credit: credit(byte.into()),
            amount: usdc(100),
            block: BlockNumber::new(5),
        },
        at(3),
    )
    .unwrap();
    r#ref
}

fn fees(count: u64) -> TokenAmount {
    TokenAmount::from(count * FEE)
}

/// `usdc(whole)` less `count` ckUSDC fees: what a payout moves once the user's own fees
/// are taken out of it (P11d: the user pays every fee).
fn less(whole: u64, count: u64) -> TokenAmount {
    usdc(whole).checked_sub(fees(count)).unwrap()
}

/// Review fix 4: the pool's own refusal after `SwapBegun`. Its ckUSDC left at the pool is
/// withdrawn back, the swap undone, and all of it paid as ckUSDC; with nothing at the pool
/// the deposit never happened, and all of it is paid as ckUSDC too. What the leg cost is in
/// what the payout spent.
#[test]
fn a_refused_swap_is_undone_and_paid_all_as_ckusdc() {
    on_fresh_memory(|| {
        crate::storage::init();
        let r#ref = arrived_swap(token(), 5_000);
        let fake = Fake::default();
        fake.quote.set(Some(TokenAmount::from(8_000_u32)));
        fake.swap_refused.set(true);
        // the pool took the deposit, then refused the swap: the ckUSDC stays there
        fake.swap_leaves.set(Some((less(99, 2), TokenAmount::ZERO)));
        block_on(drive_deposit(&fake, r#ref, clock, &open_pool)).unwrap();
        assert_eq!(
            *fake.calls.borrow(),
            vec![
                format!("quote {}", less(99, 2)),
                "unused".to_string(),
                format!("approve {}", less(99, 1)),
                format!("swap {} at least 7000", less(99, 2)),
                "unused".to_string(),
                format!("withdraw ckUSDC {} fee {FEE}", less(99, 2)),
                format!("transfer {}", less(99, 4)),
            ]
        );
        assert_eq!(fake.transfers(), vec![(ckusdc(), account(1), less(99, 4))]);
        let paid = deposit(&r#ref).unwrap();
        assert_eq!(paid.status, DepositStatus::Paid);
        // the deposit's pull and the withdrawal's transfer
        assert_eq!(paid.payout.swap.undone, Some(fees(2)));
        assert!(paid.payout.fell_back.is_some());
        // the user's 99, out of which the transfer's fee, the approval's, and the undone
        // swap's two
        assert_eq!(paid.paid.unwrap().spent, usdc(99));

        // refused before its deposit: nothing at the pool, nothing withdrawn
        let other = arrive(2, token(), 5_000);
        let fake = Fake::default();
        fake.quote.set(Some(TokenAmount::from(8_000_u32)));
        fake.swap_refused.set(true);
        block_on(drive_deposit(&fake, other, clock, &open_pool)).unwrap();
        assert!(!fake
            .calls
            .borrow()
            .iter()
            .any(|call| call.starts_with("withdraw")));
        assert_eq!(fake.transfers(), vec![(ckusdc(), account(2), less(99, 2))]);
        let paid = deposit(&other).unwrap();
        assert_eq!(paid.payout.swap.undone, Some(TokenAmount::ZERO));
        assert_eq!(paid.paid.unwrap().spent, usdc(99));
        assert_eq!(ledger().pockets.icp.reserved, TokenAmount::ZERO);
    });
}

/// Review fix 4: a swap whose answer never came is settled by what the pool holds of the
/// canister's: its output there is withdrawn and paid; all of its ckUSDC there is withdrawn
/// back and paid as ckUSDC; anything else is unknown, and nothing is withdrawn.
#[test]
fn a_swap_with_no_answer_is_settled_by_what_the_pool_holds() {
    on_fresh_memory(|| {
        crate::storage::init();
        let swapped = arrived_swap(token(), 5_000);
        let fake = Fake::default();
        fake.quote.set(Some(TokenAmount::from(8_000_u32)));
        fake.swap_leaves
            .set(Some((TokenAmount::ZERO, TokenAmount::from(7_900_u32))));
        block_on(drive_deposit(&fake, swapped, clock, &open_pool)).unwrap();
        assert!(fake
            .calls
            .borrow()
            .contains(&format!("withdraw token 7900 fee {TOKEN_FEE}")));
        assert_eq!(
            fake.transfers(),
            vec![(token(), account(1), TokenAmount::from(5_900_u32))]
        );
        let paid = deposit(&swapped).unwrap();
        assert_eq!(paid.status, DepositStatus::Paid);
        assert_eq!(paid.payout.swap.out, Some(TokenAmount::from(7_900_u32)));
        assert_eq!(paid.paid.unwrap().spent, usdc(99));

        let not_swapped = arrive(2, token(), 5_000);
        let fake = Fake::default();
        fake.quote.set(Some(TokenAmount::from(8_000_u32)));
        fake.swap_leaves.set(Some((less(99, 2), TokenAmount::ZERO)));
        block_on(drive_deposit(&fake, not_swapped, clock, &open_pool)).unwrap();
        assert_eq!(fake.transfers(), vec![(ckusdc(), account(2), less(99, 4))]);
        assert_eq!(
            deposit(&not_swapped).unwrap().payout.swap.undone,
            Some(fees(2))
        );

        // part of its amount in at the pool, after an answer that never came: unknown
        let unknown = arrive(3, token(), 5_000);
        let fake = Fake::default();
        fake.quote.set(Some(TokenAmount::from(8_000_u32)));
        fake.swap_leaves.set(Some((usdc(50), TokenAmount::ZERO)));
        assert_eq!(
            block_on(drive_deposit(&fake, unknown, clock, &open_pool)),
            Err(Stop::Failed(
                "the ICPSwap swap's outcome is unknown".to_string()
            ))
        );
        assert!(!fake
            .calls
            .borrow()
            .iter()
            .any(|call| call.starts_with("withdraw")));
        assert!(fake.transfers().is_empty());
    });
}

/// Review fix 9: one leg at a time at an ICPSwap pool. A payout whose pool another leg holds
/// waits, calls nothing and counts nothing; once the pool is free it goes on.
#[test]
fn two_legs_never_work_one_icpswap_pool_at_once() {
    on_fresh_memory(|| {
        crate::storage::init();
        let r#ref = arrived_swap(token(), 5_000);
        let fake = Fake::default();
        fake.quote.set(Some(TokenAmount::from(8_000_u32)));
        fake.swap_out.set(Some(TokenAmount::from(7_900_u32)));
        let held = Leg::take(icpswap_pool()).unwrap();
        // a refused taker never releases the holder's pool
        assert!(Leg::take(icpswap_pool()).is_none());
        assert!(Leg::take(icpswap_pool()).is_none());
        let stop = block_on(drive_deposit(&fake, r#ref, clock, &open_pool)).unwrap_err();
        assert!(matches!(stop, Stop::Retry(why) if why.contains("another payout's leg")));
        assert!(fake.calls.borrow().is_empty());
        assert_eq!(deposit(&r#ref).unwrap().payout.failures, 0);
        drop(held);
        block_on(drive_deposit(&fake, r#ref, clock, &open_pool)).unwrap();
        assert_eq!(deposit(&r#ref).unwrap().status, DepositStatus::Paid);
        assert!(Leg::take(icpswap_pool()).is_some(), "released once paid");
    });
}

/// Found by the test above: a refused taker of a payout's guard built a guard and dropped
/// it, which released the holder's id, so a third taker ran beside the first. It holds now.
#[test]
fn a_refused_taker_never_releases_a_busy_payout() {
    let held = Busy::take([0x42; 32]).unwrap();
    assert!(Busy::take([0x42; 32]).is_none());
    assert!(Busy::take([0x42; 32]).is_none(), "still held");
    drop(held);
    assert!(Busy::take([0x42; 32]).is_some());
}

/// Review fix 9: every approval's fee is the payout's: a leg approved, then stopped by the
/// pool's guard, approves again on the next drive, and both fees are in what it spent. The
/// second approval's fee came out of what the leg deposited (P11e), so what the payout spent
/// is what left the account: the deposit the leg recorded, not the amount it was handed
/// (CR-09), the user's 99 in all.
#[test]
fn every_approval_is_counted_in_what_the_payout_spent() {
    on_fresh_memory(|| {
        crate::storage::init();
        let r#ref = arrived_swap(token(), 5_000);
        let fake = Fake::default();
        fake.quote.set(Some(TokenAmount::from(8_000_u32)));
        fake.swap_out.set(Some(TokenAmount::from(7_900_u32)));
        let reads = Cell::new(0);
        let open_once = || {
            reads.set(reads.get() + 1);
            if reads.get() == 1 {
                Ok(())
            } else {
                Err("a controller paused the pool".to_string())
            }
        };
        let stop = block_on(drive_deposit(&fake, r#ref, clock, &open_once)).unwrap_err();
        assert!(matches!(stop, Stop::Retry(_)));
        assert_eq!(deposit(&r#ref).unwrap().payout.swap.approvals, Some(1));
        assert!(!deposit(&r#ref).unwrap().payout.swap.begun);
        block_on(drive_deposit(&fake, r#ref, clock, &open_pool)).unwrap();
        let paid = deposit(&r#ref).unwrap();
        assert_eq!(paid.payout.swap.approvals, Some(2));
        // its two approvals, the pool's pull and the deposit it recorded, one fee less for
        // the approval asked again
        let leg = paid.payout.swap;
        assert_eq!(
            paid.paid.unwrap().spent,
            leg.amount_in.unwrap().checked_add(fees(3)).unwrap()
        );
        assert_eq!(paid.paid.unwrap().spent, usdc(99));
    });
}

/// Review fix 12: a token no longer listed with its pool before the swap began falls back
/// to ckUSDC, all of it, without asking the pool anything.
#[test]
fn a_token_delisted_before_its_swap_falls_back() {
    on_fresh_memory(|| {
        crate::storage::init();
        let r#ref = arrived_swap(token(), 5_000);
        record_at(
            PoolStep::PayoutStarted {
                r#ref,
                attempt: PayoutAttempt {
                    created_at: at(4),
                    memo: r#ref.into_bytes(),
                    path: PayoutPath::Icpswap {
                        pool: icpswap_pool(),
                    },
                    amount: Some(less(99, 2)),
                    token: Some(PrincipalBytes(ckusdc())),
                },
            },
            at(4),
        )
        .unwrap();
        record_at(
            PoolStep::SettingsSet {
                settings: settings(),
            },
            at(5),
        )
        .unwrap();
        let fake = Fake::default();
        block_on(drive_deposit(&fake, r#ref, clock, &open_pool)).unwrap();
        assert_eq!(
            *fake.calls.borrow(),
            vec![format!("transfer {}", less(99, 1))]
        );
        let paid = deposit(&r#ref).unwrap();
        assert_eq!(paid.paid.unwrap().token, ckusdc());
        assert!(paid
            .payout
            .fell_back
            .is_some_and(|why| why.contains("no longer listed")));
    });
}

/// Review fix 8: the exact transfer is fixed when it is first known and every retry reuses
/// it, whatever the settings say by then: a withdrawal's ckUSDC amount from its start, a
/// swap's output transfer from its `Swapped`.
#[test]
fn a_retry_reuses_the_transfer_its_payout_fixed() {
    on_fresh_memory(|| {
        crate::storage::init();
        let r#ref = arrived_swap(token(), 5_000);
        let owner = account(200);
        let id = PoolOpId::new([0x61; 32]);
        let pool = ledger();
        let burned = Shares::burn_for(usdc(100), pool.total_assets(), pool.total_shares).unwrap();
        let fixed = usdc(100).checked_sub(fees(1)).unwrap();
        record_at(
            PoolStep::WithdrawStarted {
                id,
                owner,
                burned,
                usdc: usdc(100),
                to: owner,
                token_out: ckusdc(),
                min_out: 0,
                attempt: PayoutAttempt {
                    created_at: at(4),
                    memo: id.into_bytes(),
                    path: PayoutPath::Ckusdc,
                    amount: Some(fixed),
                    token: Some(PrincipalBytes(ckusdc())),
                },
            },
            at(4),
        )
        .unwrap();
        let fake = Fake::default();
        fake.quote.set(Some(TokenAmount::from(8_000_u32)));
        fake.swap_out.set(Some(TokenAmount::from(7_900_u32)));
        // both transfers' answers are lost
        fake.lose_answers.set(2);
        assert!(matches!(
            block_on(drive_withdrawal(&fake, id, clock, &open_pool)),
            Err(Stop::Retry(_))
        ));
        assert!(matches!(
            block_on(drive_deposit(&fake, r#ref, clock, &open_pool)),
            Err(Stop::Retry(_))
        ));
        // the fees change before the retries
        let mut dearer = settings();
        dearer.icp_tokens = vec![IcpToken {
            ledger: token(),
            icpswap_pool: icpswap_pool(),
        }];
        dearer.ledger_fees = vec![
            LedgerFee {
                ledger: ckusdc(),
                fee: fees(3),
            },
            LedgerFee {
                ledger: token(),
                fee: TokenAmount::from(3 * TOKEN_FEE),
            },
        ];
        record_at(PoolStep::SettingsSet { settings: dearer }, at(6)).unwrap();
        assert!(block_on(drive_withdrawal(&fake, id, clock, &open_pool))
            .unwrap()
            .is_some());
        block_on(drive_deposit(&fake, r#ref, clock, &open_pool)).unwrap();
        let transfers: Vec<String> = fake
            .calls
            .borrow()
            .iter()
            .filter(|call| call.starts_with("transfer"))
            .cloned()
            .collect();
        assert_eq!(
            transfers,
            vec![
                format!("transfer {fixed}"),
                "transfer 5900".to_string(),
                format!("transfer {fixed}"),
                "transfer 5900".to_string(),
            ]
        );
        assert_eq!(
            fake.transfers(),
            vec![
                (ckusdc(), owner, fixed),
                (token(), account(1), TokenAmount::from(5_900_u32)),
            ]
        );
    });
}

/// Review (payout, upgrade): a payout started before this build (its attempt with no amount
/// or ledger, its output with no transfer fixed, its approvals not counted) is still driven
/// to its end after the upgrade, its amounts worked out as they were then.
#[test]
fn a_payout_started_before_the_upgrade_is_driven_to_its_end() {
    on_fresh_memory(|| {
        crate::storage::init();
        let old_attempt = |r#ref: PoolRef, path: PayoutPath| PayoutAttempt {
            created_at: at(4),
            memo: r#ref.into_bytes(),
            path,
            amount: None,
            token: None,
        };
        // a ckUSDC payout started
        let ckusdc_payout = arrived_swap(ckusdc(), 1);
        record_at(
            PoolStep::PayoutStarted {
                r#ref: ckusdc_payout,
                attempt: old_attempt(ckusdc_payout, PayoutPath::Ckusdc),
            },
            at(4),
        )
        .unwrap();
        // a token payout swapped, its output not sent yet
        let swapped = arrive(2, token(), 5_000);
        let path = PayoutPath::Icpswap {
            pool: icpswap_pool(),
        };
        record_at(
            PoolStep::PayoutStarted {
                r#ref: swapped,
                attempt: old_attempt(swapped, path),
            },
            at(4),
        )
        .unwrap();
        record_at(
            PoolStep::SwapBegun {
                r#ref: swapped,
                before: None,
                amount_in: None,
                leg: None,
            },
            at(4),
        )
        .unwrap();
        record_at(
            PoolStep::Swapped {
                r#ref: swapped,
                out: TokenAmount::from(7_900_u32),
                send: None,
                leg: None,
            },
            at(4),
        )
        .unwrap();
        // a token payout started, its leg not begun
        let not_begun = arrive(3, token(), 5_000);
        record_at(
            PoolStep::PayoutStarted {
                r#ref: not_begun,
                attempt: old_attempt(not_begun, path),
            },
            at(4),
        )
        .unwrap();
        // a withdrawal started
        let owner = account(200);
        let id = PoolOpId::new([0x61; 32]);
        let pool = ledger();
        let burned = Shares::burn_for(usdc(100), pool.total_assets(), pool.total_shares).unwrap();
        record_at(
            PoolStep::WithdrawStarted {
                id,
                owner,
                burned,
                usdc: usdc(100),
                to: owner,
                token_out: ckusdc(),
                min_out: 0,
                attempt: PayoutAttempt {
                    created_at: at(4),
                    memo: id.into_bytes(),
                    path: PayoutPath::Ckusdc,
                    amount: None,
                    token: None,
                },
            },
            at(4),
        )
        .unwrap();

        let fake = Fake::default();
        fake.quote.set(Some(TokenAmount::from(8_000_u32)));
        fake.swap_out.set(Some(TokenAmount::from(7_900_u32)));
        block_on(drive_with(&fake, clock, &open_pool));
        let mut paid = fake.transfers();
        paid.sort_by_key(|(_, to, _)| to.owner);
        assert_eq!(
            paid,
            vec![
                (ckusdc(), account(1), less(99, 1)),
                // the output less the token's fee, as an output was sent then
                (token(), account(2), TokenAmount::from(6_900_u32)),
                // swapped now, under this build's rule
                (token(), account(3), TokenAmount::from(5_900_u32)),
                (ckusdc(), owner, usdc(100).checked_sub(fees(1)).unwrap()),
            ]
        );
        for r#ref in [ckusdc_payout, swapped, not_begun] {
            assert_eq!(deposit(&r#ref).unwrap().status, DepositStatus::Paid);
        }
        // the old leg's one approval, presumed, is in what it spent: the user's 99
        assert_eq!(deposit(&swapped).unwrap().paid.unwrap().spent, usdc(99));
        assert!(ledger().pending.is_empty());
        assert_eq!(ledger().pockets.icp.reserved, TokenAmount::ZERO);
    });
}

/// Review (payout, fund misattribution): the canister's unused balance at an ICPSwap pool is
/// one balance for every leg there. A leg's is only what the pool holds above what it held
/// just before that leg's swap: a leg whose swap did not happen withdraws its own deposit
/// and nothing of what the pool already held, and while it is not settled no other leg
/// begins there; the other leg then completes and the pool's other holdings are untouched.
#[test]
fn two_legs_at_one_pool_each_withdraw_only_their_own() {
    on_fresh_memory(|| {
        crate::storage::init();
        let first = arrived_swap(token(), 5_000);
        let second = arrive(2, token(), 5_000);
        let fake = Fake::default();
        fake.quote.set(Some(TokenAmount::from(8_000_u32)));
        // 50 of a payout stopped for a human already sits at the pool: never anyone else's
        let theirs = (usdc(50), TokenAmount::ZERO);
        fake.unused.set(Some(theirs));
        // the first leg's swap: no answer, and nothing of it at the pool yet
        assert_eq!(
            block_on(drive_deposit(&fake, first, clock, &open_pool)),
            Err(Stop::Failed(
                "the ICPSwap swap's outcome is unknown".to_string()
            ))
        );
        assert_eq!(
            deposit(&first).unwrap().payout.swap.before,
            Some(PoolHeld {
                ckusdc: usdc(50),
                token: TokenAmount::ZERO,
            })
        );
        // while it is not settled, the second leg does not begin at the pool
        fake.calls.borrow_mut().clear();
        let stop = block_on(drive_deposit(&fake, second, clock, &open_pool)).unwrap_err();
        assert!(matches!(stop, Stop::Retry(why) if why.contains("not settled")));
        assert!(!fake
            .calls
            .borrow()
            .iter()
            .any(|call| call.starts_with("approve") || call.starts_with("swap")));
        // the first leg's deposit shows at the pool: it withdraws its 98.98 (the user's 99
        // less the approval's and the pull's fees), not the 148.98 there
        let swap_in = less(99, 2);
        fake.unused.set(Some((
            usdc(50).checked_add(swap_in).unwrap(),
            TokenAmount::ZERO,
        )));
        block_on(drive_deposit(&fake, first, clock, &open_pool)).unwrap();
        assert!(fake
            .calls
            .borrow()
            .contains(&format!("withdraw ckUSDC {swap_in} fee {FEE}")));
        let undone = deposit(&first).unwrap();
        assert_eq!(undone.status, DepositStatus::Paid);
        assert_eq!(undone.payout.swap.withdrawn, Some(swap_in));
        assert_eq!(fake.unused.get(), Some(theirs));
        // the second leg completes, and withdraws nothing
        fake.calls.borrow_mut().clear();
        fake.swap_out.set(Some(TokenAmount::from(7_900_u32)));
        block_on(drive_deposit(&fake, second, clock, &open_pool)).unwrap();
        assert!(!fake
            .calls
            .borrow()
            .iter()
            .any(|call| call.starts_with("withdraw")));
        assert_eq!(deposit(&second).unwrap().status, DepositStatus::Paid);
        assert_eq!(fake.unused.get(), Some(theirs));
        assert_eq!(
            fake.transfers(),
            vec![
                (ckusdc(), account(1), less(99, 4)),
                (token(), account(2), TokenAmount::from(5_900_u32)),
            ]
        );
    });
}

/// Review (payout, slippage): no path asks the pool for less than `min_out` and the
/// token's fee twice, and no output is paid below `min_out`: an answer below the minimum it
/// was asked for is not paid, and waits for a human.
#[test]
fn no_output_is_paid_below_min_out() {
    on_fresh_memory(|| {
        crate::storage::init();
        let r#ref = arrived_swap(token(), 5_000);
        let fake = Fake::default();
        fake.quote.set(Some(TokenAmount::from(8_000_u32)));
        // the pool breaks its own minimum: 6500 pays 4500 after the two fees
        fake.swap_out.set(Some(TokenAmount::from(6_500_u32)));
        let stop = block_on(drive_deposit(&fake, r#ref, clock, &open_pool)).unwrap_err();
        assert!(matches!(stop, Stop::Failed(why) if why.contains("below the 5000")));
        assert!(fake
            .calls
            .borrow()
            .contains(&format!("swap {} at least 7000", less(99, 2))));
        assert!(fake.transfers().is_empty());
        // asked again, still not paid
        assert!(matches!(
            block_on(drive_deposit(&fake, r#ref, clock, &open_pool)),
            Err(Stop::Failed(_))
        ));
        assert!(fake.transfers().is_empty());
    });
}

// ---- routes through ICP (P11b Task 1b) ----

fn icp_ledger() -> Principal {
    Principal::from_text("ryjl3-tyaaa-aaaaa-aaaba-cai").unwrap()
}

fn icp_pool() -> Principal {
    Principal::from_slice(&[0x81; 10])
}

fn token_icp_pool() -> Principal {
    Principal::from_slice(&[0x82; 10])
}

const ICP_FEE: u64 = 10;

/// ICPSwap pools one by one, and a ledger that executes a transfer once per dedup pair: each
/// pool its own quote, its own swap answer and its own unused balances (token in, token out
/// of the leg asked, as a pool keyed by its pair would).
#[derive(Default)]
struct Pools {
    calls: RefCell<Vec<String>>,
    executed: RefCell<BTreeMap<TransferKey, Executed>>,
    /// Each pool's quotes, one per call, the last one kept.
    quotes: RefCell<BTreeMap<Principal, Vec<TokenAmount>>>,
    /// None: the pool answers its own `err`
    outs: RefCell<BTreeMap<Principal, Option<TokenAmount>>>,
}

impl Pools {
    fn transfers(&self) -> Vec<(Principal, IcpAccount, TokenAmount)> {
        self.executed
            .borrow()
            .iter()
            .map(|((ledger, _, _), (to, amount, _))| (*ledger, *to, *amount))
            .collect()
    }

    fn name(pool: Principal) -> &'static str {
        if pool == icp_pool() {
            "icp/ckusdc"
        } else {
            "token/icp"
        }
    }
}

impl Ledgers for Pools {
    async fn transfer(
        &self,
        ledger: Principal,
        to: IcpAccount,
        amount: TokenAmount,
        dedup: Dedup,
    ) -> Result<u64, CallError> {
        self.calls.borrow_mut().push(format!("transfer {amount}"));
        let key = (ledger, dedup.memo, dedup.created_at_nanos);
        let mut executed = self.executed.borrow_mut();
        if let Some((_, _, block)) = executed.get(&key) {
            return Ok(*block);
        }
        let block = executed.len() as u64 + 100;
        executed.insert(key, (to, amount, block));
        Ok(block)
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
        spender: Principal,
        amount: TokenAmount,
    ) -> Result<(), CallError> {
        self.calls
            .borrow_mut()
            .push(format!("approve {} {amount}", Self::name(spender)));
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
        pool: Principal,
        amount_in: TokenAmount,
        _zero_for_one: bool,
    ) -> Result<TokenAmount, CallError> {
        self.calls
            .borrow_mut()
            .push(format!("quote {} {amount_in}", Self::name(pool)));
        let mut quotes = self.quotes.borrow_mut();
        let Some(answers) = quotes.get_mut(&pool) else {
            return Ok(TokenAmount::ZERO);
        };
        Ok(if answers.len() > 1 {
            answers.remove(0)
        } else {
            answers[0]
        })
    }

    async fn swap(
        &self,
        pool: Principal,
        amount_in: TokenAmount,
        _zero_for_one: bool,
        min_out: TokenAmount,
        _fee_in: TokenAmount,
        _fee_out: TokenAmount,
    ) -> Result<TokenAmount, CallError> {
        self.calls.borrow_mut().push(format!(
            "swap {} {amount_in} at least {min_out}",
            Self::name(pool)
        ));
        match self.outs.borrow_mut().remove(&pool) {
            Some(Some(out)) => Ok(out),
            Some(None) => Err(CallError::Refused("the pool answered Err(slippage)".into())),
            None => Err(CallError::Refused("no answer set".into())),
        }
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

/// A pool with 1000 ckUSDC on ICP, `token()` routed through ICP, and a swap of 100 USDC to
/// it for `account(1)` arrived.
fn arrived_via_icp(min_out: u128) -> PoolRef {
    funded_pool(usdc(1_000));
    record_at(
        PoolStep::IcpRoutesSet {
            routes: types::pool::IcpRoutes {
                icp_ckusdc_pool: Some(PrincipalBytes(icp_pool())),
                icp_ledger: Some(PrincipalBytes(icp_ledger())),
                routes: vec![
                    types::pool::IcpRoute {
                        ledger: icp_ledger(),
                        via: types::pool::IcpRouteVia::Direct(icp_pool()),
                    },
                    types::pool::IcpRoute {
                        ledger: token(),
                        via: types::pool::IcpRouteVia::Icp(token_icp_pool()),
                    },
                ],
                ledger_fees: vec![
                    LedgerFee {
                        ledger: icp_ledger(),
                        fee: TokenAmount::from(ICP_FEE),
                    },
                    LedgerFee {
                        ledger: token(),
                        fee: TokenAmount::from(TOKEN_FEE),
                    },
                ],
            },
        },
        at(1),
    )
    .unwrap();
    arrive(1, token(), min_out)
}

/// Both legs through ICP: the pools' quotes read first, the first leg's minimum the ICP the
/// second needs, each leg recorded before its call, and the token's output to the recipient.
#[test]
fn a_route_through_icp_swaps_twice_and_pays_the_token() {
    on_fresh_memory(|| {
        crate::storage::init();
        let r#ref = arrived_via_icp(5_000);
        // the hold is the user's 99, every fee of both legs out of it (P11d)
        assert_eq!(deposit(&r#ref).unwrap().held(), usdc(99));
        let pools = Pools::default();
        pools
            .quotes
            .borrow_mut()
            .insert(icp_pool(), vec![TokenAmount::from(1_000_030_u64)]);
        pools
            .quotes
            .borrow_mut()
            .insert(token_icp_pool(), vec![TokenAmount::from(14_000_u32)]);
        pools
            .outs
            .borrow_mut()
            .insert(icp_pool(), Some(TokenAmount::from(1_000_030_u64)));
        pools
            .outs
            .borrow_mut()
            .insert(token_icp_pool(), Some(TokenAmount::from(13_000_u32)));
        block_on(drive_deposit(&pools, r#ref, clock, &open_pool)).unwrap();
        // the second leg sells what the first bought less three ICP fees: 1,000,000; it must
        // give 5,000 and two token fees, 7,000, of the 14,000 quoted: half, 500,000, and the
        // three fees back on top for the first leg's minimum
        assert_eq!(
            *pools.calls.borrow(),
            vec![
                format!("quote icp/ckusdc {}", less(99, 2)),
                "quote token/icp 1000000".to_string(),
                format!("quote icp/ckusdc {}", less(99, 2)),
                format!("approve icp/ckusdc {}", less(99, 1)),
                format!("swap icp/ckusdc {} at least 500030", less(99, 2)),
                "quote token/icp 1000000".to_string(),
                "approve token/icp 1000010".to_string(),
                "swap token/icp 1000000 at least 7000".to_string(),
                "transfer 11000".to_string(),
            ]
        );
        assert_eq!(
            pools.transfers(),
            vec![(token(), account(1), TokenAmount::from(11_000_u32))]
        );
        let paid = deposit(&r#ref).unwrap();
        assert_eq!(paid.status, DepositStatus::Paid);
        let second = paid.payout.second.unwrap();
        assert_eq!(second.out, Some(TokenAmount::from(13_000_u32)));
        assert_eq!(second.amount_in, Some(TokenAmount::from(1_000_000_u64)));
        // only the first leg cost ckUSDC: its approval, its deposit and its pull, the user's
        // 99 in all
        assert_eq!(paid.paid.unwrap().spent, usdc(99));
    });
}

/// All or nothing over both legs: a second leg that cannot reach the minimum (declined once,
/// short again) swaps the ICP back to ckUSDC on the ICP/ckUSDC pool, and ALL of it is paid as
/// ckUSDC; the round trip's loss is the user's (P11d).
#[test]
fn a_second_leg_short_pays_everything_as_ckusdc() {
    on_fresh_memory(|| {
        crate::storage::init();
        let r#ref = arrived_via_icp(5_000);
        let pools = Pools::default();
        // the ICP/ckUSDC pool: the chained check, the first leg, then the way back's quote
        pools
            .quotes
            .borrow_mut()
            .insert(icp_pool(), vec![TokenAmount::from(1_000_030_u64)]);
        // the token's pool: 14,000 when the legs are planned, then 6,999 for the second leg
        pools.quotes.borrow_mut().insert(
            token_icp_pool(),
            vec![TokenAmount::from(14_000_u32), TokenAmount::from(6_999_u32)],
        );
        pools
            .outs
            .borrow_mut()
            .insert(icp_pool(), Some(TokenAmount::from(1_000_030_u64)));
        assert_eq!(
            block_on(drive_deposit(&pools, r#ref, clock, &open_pool)),
            Err(Stop::Later)
        );
        let declined = deposit(&r#ref).unwrap().payout;
        assert!(declined.swap.out.is_some());
        assert_eq!(declined.second.unwrap().declined, 1);
        assert!(pools.transfers().is_empty());
        // short again: the ICP goes back. The canister holds what the first leg bought less
        // the withdrawal's fee; the way back sells it less its approval's and pull's fees
        pools.outs.borrow_mut().insert(icp_pool(), Some(usdc(98)));
        block_on(drive_deposit(&pools, r#ref, clock, &open_pool)).unwrap();
        let calls = pools.calls.borrow();
        assert!(calls.contains(&"swap icp/ckusdc 1000000 at least 1".to_string()));
        assert!(!calls.iter().any(|call| call.starts_with("swap token/icp")));
        // all of it, as ckUSDC, never a mix: the 98 the way back brought less its
        // withdrawal's fee and the transfer's, the first leg's 99 spent
        assert_eq!(pools.transfers(), vec![(ckusdc(), account(1), less(98, 2))]);
        let paid = deposit(&r#ref).unwrap();
        assert_eq!(paid.status, DepositStatus::Paid);
        assert!(paid.payout.fell_back.is_some());
        assert_eq!(paid.payout.back.unwrap().out, Some(usdc(98)));
        // what left the ICP pocket: the transfer and its fee, the first leg's deposit, its
        // approval and pull, less the ckUSDC the way back brought: the user's 99
        assert_eq!(paid.paid.unwrap().spent, usdc(99));
    });
}

/// A way back that fails too stops the payout for a human, as P11a does after `SwapBegun`:
/// nothing is paid, nothing is guessed.
#[test]
fn a_way_back_that_fails_stops_for_a_human() {
    on_fresh_memory(|| {
        crate::storage::init();
        let r#ref = arrived_via_icp(5_000);
        let pools = Pools::default();
        pools
            .quotes
            .borrow_mut()
            .insert(icp_pool(), vec![TokenAmount::from(1_000_030_u64)]);
        pools.quotes.borrow_mut().insert(
            token_icp_pool(),
            vec![TokenAmount::from(14_000_u32), TokenAmount::from(6_999_u32)],
        );
        pools
            .outs
            .borrow_mut()
            .insert(icp_pool(), Some(TokenAmount::from(1_000_030_u64)));
        assert_eq!(
            block_on(drive_deposit(&pools, r#ref, clock, &open_pool)),
            Err(Stop::Later)
        );
        // the way back is refused, and nothing of it is left at the pool
        pools.outs.borrow_mut().insert(icp_pool(), None);
        let stop = block_on(drive_deposit(&pools, r#ref, clock, &open_pool)).unwrap_err();
        assert!(matches!(stop, Stop::Failed(ref why) if why.starts_with("ALERT")));
        assert!(pools.transfers().is_empty());
        let payout = deposit(&r#ref).unwrap().payout;
        assert!(payout.fell_back.is_none());
        assert_eq!(
            payout.back.unwrap().undone,
            Some(TokenAmount::ZERO),
            "the pool refused before the deposit"
        );
    });
}

/// The legs are planned on both pools' quotes before anything is called: a route through ICP
/// that cannot reach the minimum declines once, then pays all of it as ckUSDC, nothing
/// approved or swapped.
#[test]
fn a_route_through_icp_short_from_the_start_pays_ckusdc_and_calls_nothing() {
    on_fresh_memory(|| {
        crate::storage::init();
        let r#ref = arrived_via_icp(5_000);
        let pools = Pools::default();
        pools
            .quotes
            .borrow_mut()
            .insert(icp_pool(), vec![TokenAmount::from(1_000_030_u64)]);
        pools
            .quotes
            .borrow_mut()
            .insert(token_icp_pool(), vec![TokenAmount::from(6_999_u32)]);
        assert_eq!(
            block_on(drive_deposit(&pools, r#ref, clock, &open_pool)),
            Err(Stop::Later)
        );
        assert_eq!(deposit(&r#ref).unwrap().payout.swap.declined, 1);
        block_on(drive_deposit(&pools, r#ref, clock, &open_pool)).unwrap();
        assert_eq!(pools.transfers(), vec![(ckusdc(), account(1), less(99, 1))]);
        assert!(!pools
            .calls
            .borrow()
            .iter()
            .any(|call| call.starts_with("approve") || call.starts_with("swap")));
    });
}
