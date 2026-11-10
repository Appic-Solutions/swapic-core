//! The drive over everything the pool owes: each deposit's, withdrawal's and ICP stake's
//! next steps, the work one pass reads, and a payout stopped for a human.

use super::*;

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
            .ok_or_else(|| Stop::Retry("the pool has no settings".to_string()))?;
        match (&deposit.status, deposit.terms.kind) {
            (DepositStatus::Arrived, DepositKind::Stake { owner }) => {
                let minted = ledger
                    .mint_for_stake(deposit.owed(), deposit.terms.move_fee)
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
                start_swap(r#ref, &deposit, &ledger, &settings, token_out, clock)?;
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

/// An arrived swap's payout started: the hold and the dedup pair's instant are taken only
/// when the payout really starts, at the queue's head once the free ckUSDC covers it
/// (`Stop::Queued` until then).
fn start_swap(
    r#ref: PoolRef,
    deposit: &Registered,
    ledger: &types::pool::PoolLedger,
    settings: &PoolSettings,
    token_out: Principal,
    clock: impl Fn() -> Timestamp,
) -> Result<(), Stop> {
    let head = ledger.queue.head().map(|(head, _)| head);
    if head != Some(r#ref) || deposit.held() > ledger.pockets.icp.free {
        return Err(Stop::Queued);
    }
    let path = path_for(settings, token_out);
    let fee_ck = settings
        .fee_of(settings.ckusdc_ledger)
        .ok_or_else(|| Stop::Failed("no ckUSDC fee was read".to_string()))?;
    // the user pays every fee: out of `arrived - premium`
    let amount = path_amount(deposit.owed(), path, fee_ck)
        .ok_or_else(|| Stop::Failed("the amount does not cover the fees".to_string()))?;
    record(
        PoolStep::PayoutStarted {
            r#ref,
            attempt: PayoutAttempt {
                created_at: clock(),
                memo: r#ref.into_bytes(),
                path,
                amount: Some(amount),
                token: Some(PrincipalBytes(settings.ckusdc_ledger)),
            },
        },
        clock(),
    )
}

/// A queued swap's payout started now, in the drive's own message and in the queue's order:
/// `Stop::Queued` while it is not the head or the free ckUSDC does not cover it. Anything but
/// an arrived swap is left as it is.
pub fn start_queued(r#ref: PoolRef, clock: impl Fn() -> Timestamp) -> Result<(), Stop> {
    let Some(deposit) = pool_store::deposit(&r#ref) else {
        return Ok(());
    };
    let (DepositStatus::Arrived, DepositKind::Swap { token_out, .. }) =
        (&deposit.status, deposit.terms.kind)
    else {
        return Ok(());
    };
    let ledger = pool_store::ledger();
    let settings = ledger
        .settings
        .clone()
        .ok_or_else(|| Stop::Retry("the pool has no settings".to_string()))?;
    start_swap(r#ref, &deposit, &ledger, &settings, token_out, clock)
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
        move_fee,
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
            // priced after the await, on the pool as it is now, in this message, on what is
            // left of the stake once Swapic's move fee and the pool fee are taken
            let shares = pool_store::ledger()
                .mint_for_stake(amount, move_fee)
                .ok_or_else(|| Stop::Failed("the stake mints nothing".to_string()))?;
            record(
                PoolStep::StakedIcp {
                    id,
                    owner,
                    amount,
                    move_fee,
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
        // waits for a human (`resolve_stake`)
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
            "ALERT: the pool payout {id} stopped for a human after its tenth failure (resolve_deposit): {why}"
        ));
    }
}

/// How long a ledger deduplicates a transfer (the fold's own rule reads it too).
pub use types::pool::DEDUP_WINDOW;

/// The pool's guard, as the payouts read it before every outflow.
pub fn guard() -> Result<(), String> {
    crate::pool::pool_closed().map_or(Ok(()), Err)
}


/// One line for ops when the queue's head has waited longer than one books interval for
/// the ICP pocket to cover it: what the urgent rebalance is for.
fn alert_if_waited(now: Timestamp) {
    let ledger = pool_store::ledger();
    let (Some((head, _)), Some(since), Some(need)) =
        (ledger.queue.head(), ledger.head_since, ledger.queue_need())
    else {
        return;
    };
    let interval = crate::storage::config::get().books_check_interval.0;
    if now.saturating_duration_since(since) <= interval {
        return;
    }
    // the head already alerted on is kept in stable memory: one line per head, an
    // upgrade included
    if crate::storage::timer_state::get().alerted_head == Some(head) {
        return;
    }
    crate::storage::timer_state::update(|state| {
        state.alerted_head = Some(head);
        ((), true)
    });
    crate::pool::log(&format!(
        "ALERT: the pool payout {head} has waited {:?} for the ICP pocket, which is {need} short",
        now.saturating_duration_since(since)
    ));
}

/// One payout's next step, as a drive picks it: run as a task of its own (`pool::jobs`), or
/// in turn by [`drive_with`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Job {
    /// An ICP stake's pull, asked again.
    PullStake(PoolOpId),
    /// A deposit's payout or an arrived stake's mint (`drive_deposit`).
    Deposit(PoolRef),
    /// An ICP withdrawal's payout.
    Withdrawal(PoolOpId),
}

/// Picks what the pool owes that can move now, in the order one pass always took it, and hands
/// each with its hold to `start`, which answers false when there is no room: the pick stops
/// there, and what it left is picked up by the drive the next ending task arms. What moves
/// nothing out is picked whatever the pool's guard says: an ICP stake's pull asked again, an
/// arrived stake's mint. Then, while the pool is open: the payouts already started, the queued
/// ones first in first out, each started here in the queue's order (stopping at the first the
/// ICP pocket does not cover, which waits and is alerted on), and the withdrawals. A head that
/// waits holds back only the payouts queued behind it. No await: the steps' calls are the
/// tasks'. Answers how many queued payouts it could not start (their failures recorded).
pub fn pick(
    clock: impl Fn() -> Timestamp + Copy,
    guard: &impl Fn() -> Result<(), String>,
    start: &mut impl FnMut(Busy, Job) -> bool,
) -> usize {
    let now = clock();
    let work = read_state(|state| Work::of(state, now));
    let mut acted = 0;
    for id in work.pulls {
        let Some(busy) = Busy::take(id.into_bytes()) else {
            continue;
        };
        if !start(busy, Job::PullStake(id)) {
            return acted;
        }
    }
    for r#ref in work.mints {
        let Some(busy) = Busy::take(r#ref.into_bytes()) else {
            continue;
        };
        if !start(busy, Job::Deposit(r#ref)) {
            return acted;
        }
    }
    if guard().is_err() {
        return acted;
    }
    for r#ref in work.started {
        let Some(busy) = Busy::take(r#ref.into_bytes()) else {
            continue;
        };
        if !start(busy, Job::Deposit(r#ref)) {
            return acted;
        }
    }
    for r#ref in work.queue {
        if guard().is_err() {
            return acted;
        }
        let Some(busy) = Busy::take(r#ref.into_bytes()) else {
            break;
        };
        match start_queued(r#ref, clock) {
            Ok(()) => {}
            // first in first out: nothing behind a queued payout starts ahead of it
            Err(Stop::Queued) => {
                alert_if_waited(now);
                break;
            }
            Err(stop) => {
                acted += tally(Some(stop), r#ref, true, clock);
                continue;
            }
        }
        if !start(busy, Job::Deposit(r#ref)) {
            return acted;
        }
    }
    for id in work.withdrawals {
        if guard().is_err() {
            return acted;
        }
        let Some(busy) = Busy::take(id.into_bytes()) else {
            continue;
        };
        if !start(busy, Job::Withdrawal(id)) {
            return acted;
        }
    }
    acted
}

/// What one step's ending counts: 1 when it moved, else nothing, its failure recorded when
/// it `counts` (the tenth stops it for a human) and logged otherwise.
fn tally(stop: Option<Stop>, id: PoolRef, counts: bool, clock: impl Fn() -> Timestamp) -> usize {
    match stop {
        None | Some(Stop::Later) | Some(Stop::Again) | Some(Stop::Queued) => 1,
        Some(Stop::Failed(why)) if counts => {
            fail(id, why, clock());
            0
        }
        Some(Stop::Failed(why)) | Some(Stop::Append(why)) | Some(Stop::Retry(why)) => {
            crate::pool::log(&format!("the pool's {id} waits: {why}"));
            0
        }
    }
}

/// Runs one picked step: answers 1 when it moved.
pub async fn run(
    ledgers: &impl Ledgers,
    job: Job,
    clock: impl Fn() -> Timestamp + Copy,
    guard: &impl Fn() -> Result<(), String>,
) -> usize {
    match job {
        Job::PullStake(id) => tally(
            pull_stake(ledgers, id, clock).await.err(),
            payout_ref(id),
            false,
            clock,
        ),
        Job::Deposit(r#ref) => tally(
            drive_deposit(ledgers, r#ref, clock, guard).await.err(),
            r#ref,
            true,
            clock,
        ),
        Job::Withdrawal(id) => tally(
            drive_withdrawal(ledgers, id, clock, guard).await.err(),
            payout_ref(id),
            true,
            clock,
        ),
    }
}

/// One pass over everything the pool owes, each picked step run in turn: what a drive does,
/// with the steps one after another instead of as tasks of their own.
#[cfg(test)]
pub async fn drive_with(
    ledgers: &impl Ledgers,
    clock: impl Fn() -> Timestamp + Copy,
    guard: &impl Fn() -> Result<(), String>,
) -> usize {
    let mut jobs = Vec::new();
    let mut acted = pick(clock, guard, &mut |busy, job| {
        jobs.push((busy, job));
        true
    });
    for (_busy, job) in jobs {
        acted += run(ledgers, job, clock, guard).await;
    }
    acted
}

/// What one drive works through, read once at its start.
#[derive(Default)]
pub struct Work {
    pulls: Vec<PoolOpId>,
    mints: Vec<PoolRef>,
    started: Vec<PoolRef>,
    queue: Vec<PoolRef>,
    withdrawals: Vec<PoolOpId>,
}

impl Work {
    /// What the pool owes at `now` that a drive acts on: the ICP stakes' pulls young enough
    /// to ask again, the arrived stakes' mints, the swaps' payouts started, the queue, and
    /// the withdrawals. Whatever is held for a human is left out.
    pub fn of<S: Store>(state: &State<S>, now: Timestamp) -> Self {
        let ledger = state.store().pool_ledger();
        let mut work = Work::default();
        for (id, op) in &ledger.pending {
            match op {
                // a pull too old to deduplicate is a human's (`resolve_stake`)
                PendingOp::Stake { created_at, .. }
                    if now.saturating_duration_since(*created_at) < DEDUP_WINDOW =>
                {
                    work.pulls.push(*id)
                }
                PendingOp::Stake { .. } => {}
                PendingOp::Withdraw { payout, .. } if payout.failed.is_none() => {
                    work.withdrawals.push(*id)
                }
                PendingOp::Withdraw { .. } => {}
            }
        }
        // a payout stopped for a human stays in `paying` for the ICP check, and is not driven
        for r#ref in &ledger.paying {
            let Some(deposit) = state.store().pool_deposit(r#ref) else {
                continue;
            };
            match (&deposit.status, deposit.terms.kind.is_swap()) {
                (DepositStatus::Arrived, false) => work.mints.push(*r#ref),
                (DepositStatus::Paying, true) => work.started.push(*r#ref),
                _ => {}
            }
        }
        work.queue = ledger.queue.0.iter().map(|(r#ref, _)| *r#ref).collect();
        work
    }

    /// Whether a drive has nothing to act on.
    pub fn is_empty(&self) -> bool {
        self.pulls.is_empty()
            && self.mints.is_empty()
            && self.started.is_empty()
            && self.queue.is_empty()
            && self.withdrawals.is_empty()
    }
}
