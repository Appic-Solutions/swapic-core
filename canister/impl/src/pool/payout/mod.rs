//! The payout on ICP (spec 3.1 step 6, interface section 4), timer driven and idempotent:
//! every arrived swap pays its recipient `arrived - premium`, as ckUSDC or through the
//! token's ICPSwap pool, all of it in one token; every arrived stake mints its shares; every
//! ICP withdrawal pays its owner; every ICP stake whose pull's answer was lost asks again.
//!
//! The order of a payout, each line appended before the call it guards:
//! - `PayoutStarted`: the dedup pair (`created_at_time`, memo = the ref) and the path. Every
//!   ICRC transfer of the payout carries this pair, so a transfer whose answer was lost is
//!   asked again as the same transfer, and the ledger answers `Duplicate`, which is paid.
//! - ICPSwap: the pool's own quote first; below the minimum, `SwapDeclined` and once more on
//!   the next drive, then `FellBack` and ALL of it as ckUSDC (all or nothing, never a mix).
//!   Then the approval, `SwapBegun`, `depositFromAndSwap`, `Swapped` with its output, and the
//!   output's transfer. A swap begun with no answer is never called again: its outcome is
//!   unknown, and the payout stops for a human.
//! - `Paid` (or `WithdrawnIcp`), with the ckUSDC that left the pocket, every fee included.
//!
//! A refusal (or an outcome the ledger can no longer tell) is `AttemptFailed`, the payout
//! stays where it is and is driven again; at the tenth it stops for a human
//! (`PayoutFailed`). A call whose outcome is unknown records nothing and is asked again, the
//! same call: a ledger's passing trouble never stops a payout. The pool's guard runs before
//! every outflow call, after every await: a pool closed meanwhile pays nothing more.

#[cfg(test)]
mod tests;

use crate::ic::{self, Account, CallError, Dedup};
use crate::state::Store;
use crate::storage::events::read_state;
use crate::storage::pool as pool_store;
use candid::{Nat, Principal};
use std::cell::RefCell;
use std::collections::BTreeSet;
use types::hash::{PoolOpId, PoolRef};
use types::pool::{
    payout_ref, DepositKind, DepositStatus, IcpAccount, PayoutAttempt, PayoutPath, PayoutProgress,
    PendingOp, PoolSettings, PoolStep, Registered,
};
use types::{Timestamp, TokenAmount};

thread_local! {
    // Heap by necessity: the payouts a drive or a door is calling for right now. An upgrade
    // waits for every outstanding call, so nothing in here survives one.
    static BUSY: RefCell<BTreeSet<[u8; 32]>> = const { RefCell::new(BTreeSet::new()) };
}

/// Held while a drive or a door calls for one payout, released on drop, a trap included.
pub struct Busy([u8; 32]);

impl Busy {
    pub fn take(id: [u8; 32]) -> Option<Self> {
        BUSY.with(|busy| busy.borrow_mut().insert(id))
            .then_some(Self(id))
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        BUSY.with(|busy| busy.borrow_mut().remove(&self.0));
    }
}

/// The calls a payout makes, so the driver runs the same against the ledgers and a fake.
#[allow(async_fn_in_trait)]
pub trait Ledgers {
    /// `icrc1_transfer` from the canister's own account.
    async fn transfer(
        &self,
        ledger: Principal,
        to: IcpAccount,
        amount: TokenAmount,
        dedup: Dedup,
    ) -> Result<u64, CallError>;
    /// `icrc2_transfer_from` into the canister's own account, on the caller's approval of
    /// the canister's main account.
    async fn pull(
        &self,
        ledger: Principal,
        from: IcpAccount,
        amount: TokenAmount,
        dedup: Dedup,
    ) -> Result<u64, CallError>;
    async fn approve(
        &self,
        ledger: Principal,
        spender: Principal,
        amount: TokenAmount,
    ) -> Result<(), CallError>;
    /// Whether the pool sells `token_in` as its token0 (`zeroForOne`), None for a pool that
    /// does not trade the pair.
    async fn direction(
        &self,
        pool: Principal,
        token_in: Principal,
        token_out: Principal,
    ) -> Result<Option<bool>, CallError>;
    async fn quote(
        &self,
        pool: Principal,
        amount_in: TokenAmount,
        zero_for_one: bool,
    ) -> Result<TokenAmount, CallError>;
    #[allow(clippy::too_many_arguments)]
    async fn swap(
        &self,
        pool: Principal,
        amount_in: TokenAmount,
        zero_for_one: bool,
        min_out: TokenAmount,
        fee_in: TokenAmount,
        fee_out: TokenAmount,
    ) -> Result<TokenAmount, CallError>;
}

fn amount(nat: &Nat) -> TokenAmount {
    TokenAmount::try_from(nat.clone()).unwrap_or(TokenAmount::MAX)
}

/// The ledgers and ICPSwap, through `crate::ic`.
pub struct Live;

#[cfg(feature = "inttest")]
thread_local! {
    // Test-only: the next transfer's answer is lost on its way back, after the ledger
    // executed it; and how many answers were lost so.
    static LOSE_NEXT_ANSWER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static ANSWERS_LOST: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Test-only: how many transfer answers were lost on purpose.
#[cfg(feature = "inttest")]
pub fn answers_lost() -> u64 {
    ANSWERS_LOST.with(|lost| lost.get())
}

/// Test-only: loses the next transfer's answer, as a subnet that lost it would.
#[cfg(feature = "inttest")]
pub fn lose_next_answer() {
    LOSE_NEXT_ANSWER.with(|lose| lose.set(true));
}

impl Ledgers for Live {
    async fn transfer(
        &self,
        ledger: Principal,
        to: IcpAccount,
        amount: TokenAmount,
        dedup: Dedup,
    ) -> Result<u64, CallError> {
        let answer = ic::icrc1_transfer(
            ledger,
            None,
            Account::of(to.owner, to.subaccount),
            Nat::from(amount),
            dedup,
        )
        .await;
        #[cfg(feature = "inttest")]
        if answer.is_ok() && LOSE_NEXT_ANSWER.with(|lose| lose.replace(false)) {
            ANSWERS_LOST.with(|lost| lost.set(lost.get() + 1));
            return Err(CallError::Unknown("the answer was lost (test)".to_string()));
        }
        answer
    }

    async fn pull(
        &self,
        ledger: Principal,
        from: IcpAccount,
        amount: TokenAmount,
        dedup: Dedup,
    ) -> Result<u64, CallError> {
        ic::icrc2_transfer_from(
            ledger,
            None,
            Account::of(from.owner, from.subaccount),
            Account::of(ic_cdk::api::id(), None),
            Nat::from(amount),
            dedup,
        )
        .await
    }

    async fn approve(
        &self,
        ledger: Principal,
        spender: Principal,
        amount: TokenAmount,
    ) -> Result<(), CallError> {
        ic::icrc2_approve(ledger, spender, Nat::from(amount)).await
    }

    async fn direction(
        &self,
        pool: Principal,
        token_in: Principal,
        token_out: Principal,
    ) -> Result<Option<bool>, CallError> {
        let metadata = ic::pool_metadata(pool).await?;
        let (token_in, token_out) = (token_in.to_text(), token_out.to_text());
        Ok(
            if metadata.token0.address == token_in && metadata.token1.address == token_out {
                Some(true)
            } else if metadata.token1.address == token_in && metadata.token0.address == token_out {
                Some(false)
            } else {
                None
            },
        )
    }

    async fn quote(
        &self,
        pool: Principal,
        amount_in: TokenAmount,
        zero_for_one: bool,
    ) -> Result<TokenAmount, CallError> {
        ic::pool_quote(pool, &Nat::from(amount_in), zero_for_one)
            .await
            .map(|out| amount(&out))
    }

    async fn swap(
        &self,
        pool: Principal,
        amount_in: TokenAmount,
        zero_for_one: bool,
        min_out: TokenAmount,
        fee_in: TokenAmount,
        fee_out: TokenAmount,
    ) -> Result<TokenAmount, CallError> {
        ic::deposit_from_and_swap(
            pool,
            &Nat::from(amount_in),
            zero_for_one,
            &Nat::from(min_out),
            Nat::from(fee_in),
            Nat::from(fee_out),
        )
        .await
        .map(|out| amount(&out))
    }
}

/// One payout as the driver works it: a swap deposit's (the pool pays the ledger fees out
/// of the premium) or a withdrawal's (the withdrawer's amount pays them).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Payout {
    pub id: PoolRef,
    pub to: IcpAccount,
    pub token_out: Principal,
    pub min_out: u128,
    /// A deposit: what the recipient gets, `arrived - premium`. A withdrawal: the ckUSDC the
    /// shares were worth, which every fee comes out of.
    pub amount: TokenAmount,
    pub pool_pays_fees: bool,
    pub progress: PayoutProgress,
}

/// Why a payout did not finish this drive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stop {
    /// Recorded as `AttemptFailed`: driven again until the tenth.
    Failed(String),
    /// Waits for the next drive: a quote declined once was recorded.
    Later,
    /// A line was recorded that the next step reads now: a fall back to ckUSDC.
    Again,
    /// A line could not be appended: driven again.
    Append(String),
    /// Nothing to record, driven again as it is: a call whose outcome is unknown (asked again
    /// with the same dedup pair), or the pool closed before an outflow. Never counted toward
    /// the stop for a human, so a ledger's passing trouble stops no payout.
    Retry(String),
    /// Queued: not the queue's head, or the ICP pocket's free ckUSDC does not cover it yet.
    /// Nothing recorded; it starts, first in first out, once it is covered.
    Queued,
}

/// What a call's failure means for a payout: an outcome unknown is asked again as it is; a
/// refusal, and an outcome the ledger can no longer tell, count toward the stop.
fn call_stop(what: &str, error: CallError) -> Stop {
    match error {
        CallError::Unknown(_) => Stop::Retry(format!("{what}: {error}")),
        CallError::Refused(_) | CallError::Expired(_) => Stop::Failed(format!("{what}: {error}")),
    }
}

/// The pool's guard, read before every outflow call: a pool closed while a payout awaited
/// moves nothing more.
fn open(guard: &impl Fn() -> Result<(), String>) -> Result<(), Stop> {
    guard().map_err(|closed| Stop::Retry(format!("the pool is closed: {closed}")))
}

impl Payout {
    /// The ckUSDC transfer that pays all of it: what is sent, and what leaves the pocket.
    fn ckusdc(&self, fee: TokenAmount) -> Option<(TokenAmount, TokenAmount)> {
        if self.pool_pays_fees {
            Some((self.amount, self.amount.checked_add(fee)?))
        } else {
            let sent = self
                .amount
                .checked_sub(fee)
                .filter(|sent| *sent > TokenAmount::ZERO)?;
            Some((sent, self.amount))
        }
    }

    /// What the ICPSwap leg swaps, and what leaves the pocket for it: the approval's fee and
    /// the pool's pull's on top.
    fn swap_in(&self, fee: TokenAmount) -> Option<(TokenAmount, TokenAmount)> {
        let fees = fee.checked_mul(2_u8)?;
        if self.pool_pays_fees {
            Some((self.amount, self.amount.checked_add(fees)?))
        } else {
            let swap_in = self
                .amount
                .checked_sub(fees)
                .filter(|left| *left > TokenAmount::ZERO)?;
            Some((swap_in, self.amount))
        }
    }

    fn dedup(&self, attempt: &PayoutAttempt) -> Dedup {
        Dedup {
            memo: attempt.memo,
            created_at_nanos: attempt.created_at.as_nanos(),
        }
    }
}

/// The payout of an arrived swap.
pub fn of_deposit(r#ref: PoolRef, deposit: &Registered) -> Option<Payout> {
    let DepositKind::Swap {
        recipient,
        token_out,
        min_out,
    } = deposit.terms.kind
    else {
        return None;
    };
    Some(Payout {
        id: r#ref,
        to: recipient,
        token_out,
        min_out,
        amount: deposit.owed(),
        pool_pays_fees: true,
        progress: deposit.payout.clone(),
    })
}

/// The payout of a withdrawal.
pub fn of_withdrawal(id: PoolOpId, op: &PendingOp) -> Option<Payout> {
    let PendingOp::Withdraw {
        to,
        token_out,
        min_out,
        usdc,
        payout,
        ..
    } = op
    else {
        return None;
    };
    Some(Payout {
        id: payout_ref(id),
        to: *to,
        token_out: *token_out,
        min_out: *min_out,
        amount: *usdc,
        pool_pays_fees: false,
        progress: payout.clone(),
    })
}

/// The path a payout of `token_out` takes: ckUSDC for ckUSDC, the token's listed pool for
/// another, and ckUSDC for a token no longer listed.
pub fn path_for(settings: &PoolSettings, token_out: Principal) -> PayoutPath {
    match settings.pool_of(token_out) {
        Some(pool) => PayoutPath::Icpswap { pool },
        None => PayoutPath::Ckusdc,
    }
}

fn record(step: PoolStep, now: Timestamp) -> Result<(), Stop> {
    pool_store::record_at(step, now)
        .map(drop)
        .map_err(|error| Stop::Append(error.to_string()))
}

/// What a paid payout records.
pub struct Done {
    pub token: Principal,
    pub paid: TokenAmount,
    pub block: u64,
    pub spent: TokenAmount,
}

/// Drives one payout as far as it goes now, from where its progress stands: answers what was
/// paid, or why it stopped. Records every line but the last (`Paid` or `WithdrawnIcp`,
/// which the caller knows how to write).
pub async fn pay(
    ledgers: &impl Ledgers,
    settings: &PoolSettings,
    payout: Payout,
    clock: impl Fn() -> Timestamp,
    guard: &impl Fn() -> Result<(), String>,
) -> Result<Done, Stop> {
    let attempt = payout
        .progress
        .attempt
        .ok_or_else(|| Stop::Failed("the payout has no dedup pair".to_string()))?;
    let ckusdc = settings.ckusdc_ledger;
    let fee_ck = settings
        .fee_of(ckusdc)
        .ok_or_else(|| Stop::Failed("no ckUSDC fee was read".to_string()))?;
    let pool = match attempt.path {
        PayoutPath::Ckusdc => {
            let (sent, spent) = payout
                .ckusdc(fee_ck)
                .ok_or_else(|| Stop::Failed("the amount does not cover the fee".to_string()))?;
            open(guard)?;
            let block = ledgers
                .transfer(ckusdc, payout.to, sent, payout.dedup(&attempt))
                .await
                .map_err(|error| call_stop("the ckUSDC transfer", error))?;
            return Ok(Done {
                token: ckusdc,
                paid: sent,
                block,
                spent,
            });
        }
        PayoutPath::Icpswap { pool } => pool,
    };
    let fee_out = settings
        .fee_of(payout.token_out)
        .ok_or_else(|| Stop::Failed(format!("no fee was read for {}", payout.token_out)))?;
    let (swap_in, spent) = payout
        .swap_in(fee_ck)
        .ok_or_else(|| Stop::Failed("the amount does not cover the fees".to_string()))?;
    let out = match payout.progress.swap.out {
        Some(out) => out,
        None => {
            if payout.progress.swap.begun {
                return Err(Stop::Failed(
                    "the ICPSwap swap's outcome is unknown".to_string(),
                ));
            }
            // the recipient gets at least `min_out` after the token's own transfer fee
            let least = TokenAmount::from(payout.min_out)
                .checked_add(fee_out)
                .ok_or_else(|| Stop::Failed("min_out is too large".to_string()))?;
            let fall_back = |why: String| -> Result<Done, Stop> {
                record(
                    PoolStep::FellBack {
                        r#ref: payout.id,
                        why,
                    },
                    clock(),
                )?;
                Err(Stop::Again)
            };
            let zero_for_one = match ledgers
                .direction(pool, ckusdc, payout.token_out)
                .await
                .map_err(|error| call_stop("the pool's metadata", error))?
            {
                Some(direction) => direction,
                None => return fall_back("the pool does not trade the pair".to_string()),
            };
            let quoted = ledgers
                .quote(pool, swap_in, zero_for_one)
                .await
                .map_err(|error| call_stop("the pool's quote", error))?;
            if quoted < least {
                let why = format!("the pool quotes {quoted}, below the {least} needed");
                if payout.progress.swap.declined == 0 {
                    record(
                        PoolStep::SwapDeclined {
                            r#ref: payout.id,
                            why,
                        },
                        clock(),
                    )?;
                    return Err(Stop::Later);
                }
                return fall_back(why);
            }
            let allowance = swap_in
                .checked_add(fee_ck)
                .ok_or_else(|| Stop::Failed("the allowance overflows".to_string()))?;
            open(guard)?;
            ledgers
                .approve(ckusdc, pool, allowance)
                .await
                .map_err(|error| call_stop("the approval", error))?;
            open(guard)?;
            record(PoolStep::SwapBegun { r#ref: payout.id }, clock())?;
            // begun: whatever this answers, the swap is never called again
            let out = ledgers
                .swap(pool, swap_in, zero_for_one, least, fee_ck, fee_out)
                .await
                .map_err(|error| Stop::Failed(format!("the ICPSwap swap: {error}")))?;
            record(
                PoolStep::Swapped {
                    r#ref: payout.id,
                    out,
                },
                clock(),
            )?;
            out
        }
    };
    let sent = out
        .checked_sub(fee_out)
        .filter(|sent| *sent > TokenAmount::ZERO)
        .ok_or_else(|| Stop::Failed("the output does not cover its fee".to_string()))?;
    open(guard)?;
    let block = ledgers
        .transfer(payout.token_out, payout.to, sent, payout.dedup(&attempt))
        .await
        .map_err(|error| call_stop("the output's transfer", error))?;
    Ok(Done {
        token: payout.token_out,
        paid: sent,
        block,
        spent,
    })
}

/// The deposit's next steps, as far as they go now.
pub async fn drive_deposit(
    ledgers: &impl Ledgers,
    r#ref: PoolRef,
    clock: impl Fn() -> Timestamp + Copy,
    guard: &impl Fn() -> Result<(), String>,
) -> Result<(), Stop> {
    // a payout goes a few lines at most in one drive: start, the swap's, then paid
    for _ in 0..4 {
        let Some(deposit) = pool_store::deposit(&r#ref) else {
            return Ok(());
        };
        let ledger = pool_store::ledger();
        let settings = ledger
            .settings
            .clone()
            .ok_or_else(|| Stop::Failed("the pool has no settings".to_string()))?;
        match (&deposit.status, deposit.terms.kind) {
            (DepositStatus::Arrived, DepositKind::Stake { owner }) => {
                let minted = deposit
                    .owed()
                    .checked_sub(deposit.terms.rebalance_share)
                    .and_then(|amount| ledger.mint_now(amount))
                    .ok_or_else(|| Stop::Failed("the stake mints nothing".to_string()))?;
                return record(
                    PoolStep::Minted {
                        r#ref,
                        owner,
                        shares: minted,
                    },
                    clock(),
                );
            }
            (DepositStatus::Arrived, DepositKind::Swap { token_out, .. }) => {
                // the hold and the dedup pair's instant are taken only when the payout really
                // starts: the queue's head, covered by the free ckUSDC
                let head = ledger.queue.head().map(|(head, _)| head);
                if head != Some(r#ref) || deposit.held() > ledger.pockets.icp.free {
                    return Err(Stop::Queued);
                }
                record(
                    PoolStep::PayoutStarted {
                        r#ref,
                        attempt: PayoutAttempt {
                            created_at: clock(),
                            memo: r#ref.into_bytes(),
                            path: path_for(&settings, token_out),
                        },
                    },
                    clock(),
                )?;
            }
            (DepositStatus::Paying, _) => {
                let payout = of_deposit(r#ref, &deposit)
                    .ok_or_else(|| Stop::Failed("a stake has no payout".to_string()))?;
                match pay(ledgers, &settings, payout, clock, guard).await {
                    Ok(done) => {
                        return record(
                            PoolStep::Paid {
                                r#ref,
                                token: done.token,
                                amount: done.paid,
                                block: done.block,
                                spent: done.spent,
                            },
                            clock(),
                        )
                    }
                    // a fall back was recorded: pay all of it as ckUSDC now
                    Err(Stop::Again) => {}
                    Err(stop) => return Err(stop),
                }
            }
            _ => return Ok(()),
        }
    }
    Ok(())
}

/// The withdrawal's payout, as far as it goes now: answers what it burned and paid once it
/// is paid.
pub async fn drive_withdrawal(
    ledgers: &impl Ledgers,
    id: PoolOpId,
    clock: impl Fn() -> Timestamp + Copy,
    guard: &impl Fn() -> Result<(), String>,
) -> Result<Option<(types::pool::Shares, TokenAmount)>, Stop> {
    for _ in 0..3 {
        let ledger = pool_store::ledger();
        let Some(op) = ledger.pending.get(&id).cloned() else {
            return Ok(None);
        };
        let PendingOp::Withdraw {
            owner,
            burned,
            usdc,
            ..
        } = op
        else {
            return Ok(None);
        };
        let settings = ledger
            .settings
            .clone()
            .ok_or_else(|| Stop::Failed("the pool has no settings".to_string()))?;
        let payout = of_withdrawal(id, &op).expect("BUG: a withdrawal has a payout");
        match pay(ledgers, &settings, payout, clock, guard).await {
            Ok(done) => {
                record(
                    PoolStep::WithdrawnIcp {
                        id,
                        owner,
                        burned,
                        usdc,
                        token: done.token,
                        paid: done.paid,
                        block: done.block,
                        spent: done.spent,
                    },
                    clock(),
                )?;
                return Ok(Some((burned, done.paid)));
            }
            Err(Stop::Again) => {}
            Err(stop) => return Err(stop),
        }
    }
    Err(Stop::Later)
}

/// An ICP stake's pull, asked again with its own dedup pair: the shares on a landed pull, the
/// room released on a refused one.
pub async fn pull_stake(
    ledgers: &impl Ledgers,
    id: PoolOpId,
    clock: impl Fn() -> Timestamp + Copy,
) -> Result<Option<types::pool::Shares>, Stop> {
    let ledger = pool_store::ledger();
    let Some(PendingOp::Stake {
        owner,
        from,
        amount,
        created_at,
    }) = ledger.pending.get(&id).cloned()
    else {
        return Ok(None);
    };
    let settings = ledger
        .settings
        .clone()
        .ok_or_else(|| Stop::Failed("the pool has no settings".to_string()))?;
    let dedup = Dedup {
        memo: id.into_bytes(),
        created_at_nanos: created_at.as_nanos(),
    };
    match ledgers
        .pull(settings.ckusdc_ledger, from, amount, dedup)
        .await
    {
        Ok(block) => {
            // priced after the await, on the pool as it is now, in this message
            let shares = pool_store::ledger()
                .mint_now(amount)
                .ok_or_else(|| Stop::Failed("the stake mints nothing".to_string()))?;
            record(
                PoolStep::StakedIcp {
                    id,
                    owner,
                    amount,
                    rebalance_share: TokenAmount::ZERO,
                    shares,
                    block,
                },
                clock(),
            )?;
            Ok(Some(shares))
        }
        // refused: nothing moved, the room goes back
        Err(CallError::Refused(why)) => {
            record(
                PoolStep::StakeFailed {
                    id,
                    why: why.clone(),
                },
                clock(),
            )?;
            Err(Stop::Failed(format!("the ledger refused the pull: {why}")))
        }
        // too old to tell: it may have run, so it is never recorded as failed; the stake
        // waits for a human
        Err(error @ CallError::Expired(_)) => Err(Stop::Failed(format!(
            "ALERT: the pull's outcome can no longer be known: {error}"
        ))),
        Err(error @ CallError::Unknown(_)) => Err(Stop::Retry(format!("the pull: {error}"))),
    }
}

/// Records a failed drive of `id`, for the tenth to stop it.
pub fn fail(id: PoolRef, why: String, now: Timestamp) {
    if let Err(error) = pool_store::record_at(
        PoolStep::AttemptFailed {
            r#ref: id,
            why: why.clone(),
        },
        now,
    ) {
        crate::pool::log(&format!(
            "the pool payout {id} failed ({why}) and was not recorded: {error}"
        ));
        return;
    }
    let stopped = match pool_store::deposit(&id) {
        Some(deposit) => matches!(deposit.status, DepositStatus::PayoutFailed(_)),
        None => pool_store::ledger()
            .pending
            .get(&PoolOpId::new(id.into_bytes()))
            .is_some_and(
                |op| matches!(op, PendingOp::Withdraw { payout, .. } if payout.failed.is_some()),
            ),
    };
    if stopped {
        crate::pool::log(&format!(
            "ALERT: the pool payout {id} stopped for a human after its tenth failure: {why}"
        ));
    }
}

/// How long a ledger deduplicates a transfer: a pull asked again after it can no longer be
/// told from a new one, so the drive stops asking (a little before, for the clocks' skew).
pub const DEDUP_WINDOW: std::time::Duration = std::time::Duration::from_secs(23 * 3_600);

/// The pool's guard, as the payouts read it before every outflow.
pub fn guard() -> Result<(), String> {
    crate::pool::pool_closed().map_or(Ok(()), Err)
}

/// One pass over every payout the pool owes: nothing while the pool is closed.
pub async fn drive(ledgers: &impl Ledgers, clock: impl Fn() -> Timestamp + Copy) -> usize {
    drive_with(ledgers, clock, &guard).await
}

/// [`drive`] with its guard given.
pub async fn drive_with(
    ledgers: &impl Ledgers,
    clock: impl Fn() -> Timestamp + Copy,
    guard: &impl Fn() -> Result<(), String>,
) -> usize {
    if guard().is_err() {
        return 0;
    }
    let now = clock();
    let (deposits, pending): (Vec<PoolRef>, Vec<(PoolOpId, bool)>) = read_state(|state| {
        let ledger = state.store().pool_ledger();
        let pending = ledger
            .pending
            .iter()
            .filter_map(|(id, op)| match op {
                // a pull too old to deduplicate is a human's
                PendingOp::Stake { created_at, .. }
                    if now.saturating_duration_since(*created_at) < DEDUP_WINDOW =>
                {
                    Some((*id, false))
                }
                PendingOp::Stake { .. } => None,
                PendingOp::Withdraw { payout, .. } if payout.failed.is_none() => Some((*id, true)),
                PendingOp::Withdraw { .. } => None,
            })
            .collect();
        // the payouts already started and the arrived stakes; a payout stopped for a human
        // stays in `paying` for the ICP check, and is not driven
        let mut deposits = Vec::new();
        for r#ref in &ledger.paying {
            let Some(deposit) = state.store().pool_deposit(r#ref) else {
                continue;
            };
            let stake = !deposit.terms.kind.is_swap();
            if deposit.status == DepositStatus::Paying
                || (deposit.status == DepositStatus::Arrived && stake)
            {
                deposits.push(*r#ref);
            }
        }
        // then the queued payouts, first in first out
        deposits.extend(ledger.queue.0.iter().map(|(r#ref, _)| *r#ref));
        (deposits, pending)
    });
    let mut acted = 0;
    for r#ref in deposits {
        if guard().is_err() {
            break;
        }
        let Some(_busy) = Busy::take(r#ref.into_bytes()) else {
            continue;
        };
        match drive_deposit(ledgers, r#ref, clock, guard).await {
            Ok(()) => acted += 1,
            Err(Stop::Failed(why)) => fail(r#ref, why, clock()),
            Err(Stop::Later) | Err(Stop::Again) => acted += 1,
            // first in first out: nothing behind a queued payout starts ahead of it
            Err(Stop::Queued) => break,
            Err(Stop::Append(why)) | Err(Stop::Retry(why)) => {
                crate::pool::log(&format!("the pool payout {} waits: {why}", r#ref))
            }
        }
    }
    for (id, withdraw) in pending {
        if guard().is_err() {
            break;
        }
        let Some(_busy) = Busy::take(id.into_bytes()) else {
            continue;
        };
        let stop = if withdraw {
            drive_withdrawal(ledgers, id, clock, guard).await.err()
        } else {
            pull_stake(ledgers, id, clock).await.err()
        };
        match stop {
            None | Some(Stop::Later) | Some(Stop::Again) | Some(Stop::Queued) => acted += 1,
            Some(Stop::Failed(why)) if withdraw => fail(payout_ref(id), why, clock()),
            Some(Stop::Failed(why)) | Some(Stop::Append(why)) | Some(Stop::Retry(why)) => {
                crate::pool::log(&format!("the pool operation {id} waits: {why}"))
            }
        }
    }
    acted
}
