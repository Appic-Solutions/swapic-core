//! The payout on ICP (spec 3.1 step 6, interface section 4), timer driven and idempotent:
//! every arrived swap pays its recipient `arrived - premium`, as ckUSDC or through the
//! token's ICPSwap pool, all of it in one token; every arrived stake mints its shares; every
//! ICP withdrawal pays its owner; every ICP stake whose pull's answer was lost asks again.
//!
//! The order of a payout, each line appended before the call it guards:
//! - `PayoutStarted`: the dedup pair (`created_at_time`, memo = the ref) and the path. Every
//!   ICRC transfer of the payout carries this pair, so a transfer whose answer was lost is
//!   asked again as the same transfer, and the ledger answers `Duplicate`, which is paid.
//!   The attempt also fixes the exact transfer: the ckUSDC path's amount, or the ICPSwap
//!   leg's amount in, and the ckUSDC ledger. Every retry reuses them, whatever the settings
//!   say by then, so a retried transfer is the same transfer.
//! - ICPSwap, one payout at a time per ICPSwap pool (an approval sets the pool's allowance,
//!   and the canister's unused balances there are one per pool): a token no longer listed
//!   with its pool, or with no fee read, falls back; then the pool's own quote; below the
//!   minimum (`min_out` plus twice the token's fee: the pool's withdrawal to the canister and
//!   the canister's transfer each take one), `SwapDeclined` and once more on the next drive,
//!   then `FellBack` and ALL of it as ckUSDC (all or nothing, never a mix). Then the approval
//!   (`SwapApproved`, its fee counted), `SwapBegun`, `depositFromAndSwap`, `Swapped` with its
//!   output and the output's transfer fixed, and that transfer.
//! - A begun swap that did not answer with its output is never called again: what the pool
//!   holds of the canister's tells what happened. Its output there is withdrawn and paid; its
//!   ckUSDC there is withdrawn back (`SwapUndone`) and all of it paid as ckUSDC; nothing there
//!   after the pool refused the swap means the deposit never happened, and all of it is paid
//!   as ckUSDC. Only nothing there after an answer that never came stops for a human.
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
    PendingOp, PoolSettings, PoolStep, PrincipalBytes, Registered, SwapLeg,
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
        // lazily: a guard built for a refusal would release the holder's id on its drop
        BUSY.with(|busy| busy.borrow_mut().insert(id))
            .then(|| Self(id))
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        BUSY.with(|busy| busy.borrow_mut().remove(&self.0));
    }
}

thread_local! {
    // Heap by necessity: the ICPSwap pools a payout's leg is at right now. Two legs at one
    // pool would overwrite each other's approval and mix their unused balances there.
    static LEGS: RefCell<BTreeSet<Principal>> = const { RefCell::new(BTreeSet::new()) };
}

/// Held while one payout's leg is at an ICPSwap pool, released on drop, a trap included.
struct Leg(Principal);

impl Leg {
    fn take(pool: Principal) -> Option<Self> {
        // lazily: a guard built for a refusal would release the holder's pool on its drop
        LEGS.with(|legs| legs.borrow_mut().insert(pool))
            .then(|| Self(pool))
    }
}

impl Drop for Leg {
    fn drop(&mut self) {
        LEGS.with(|legs| legs.borrow_mut().remove(&self.0));
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
    /// `depositFromAndSwap`: a refusal is the pool's own `err` answer.
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
    /// The canister's unused balances at the pool, as its token0 and token1.
    async fn unused(&self, pool: Principal) -> Result<(TokenAmount, TokenAmount), CallError>;
    /// Withdraws `amount` of the canister's unused `token` at the pool to its own account,
    /// the ledger's `fee` out of it.
    async fn withdraw(
        &self,
        pool: Principal,
        token: Principal,
        amount: TokenAmount,
        fee: TokenAmount,
    ) -> Result<(), CallError>;
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

    async fn unused(&self, pool: Principal) -> Result<(TokenAmount, TokenAmount), CallError> {
        ic::pool_unused_balance(pool)
            .await
            .map(|(token0, token1)| (amount(&token0), amount(&token1)))
    }

    async fn withdraw(
        &self,
        pool: Principal,
        token: Principal,
        amount: TokenAmount,
        fee: TokenAmount,
    ) -> Result<(), CallError> {
        ic::pool_withdraw(pool, token, Nat::from(amount), Nat::from(fee))
            .await
            .map(drop)
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
    fn dedup(&self, attempt: &PayoutAttempt) -> Dedup {
        Dedup {
            memo: attempt.memo,
            created_at_nanos: attempt.created_at.as_nanos(),
        }
    }

    /// The ckUSDC transfer that pays all of a payout that fell back: a swap deposit's whole
    /// amount (the pool pays every fee); a withdrawal's amount less the transfer's fee and
    /// what its leg already cost.
    fn fallback_amount(&self, swap: &SwapLeg, fee: TokenAmount) -> Option<TokenAmount> {
        if self.pool_pays_fees {
            return Some(self.amount);
        }
        self.amount
            .checked_sub(fee)?
            .checked_sub(leg_cost(swap, None, fee)?)
            .filter(|sent| *sent > TokenAmount::ZERO)
    }
}

/// What a payout's path moves first, fixed when it starts and reused on every retry: the
/// ckUSDC transfer's amount, or the ICPSwap leg's amount in. A swap deposit's pool pays the
/// fees on top of `amount`; a withdrawal's `amount` pays them (the transfer's, or the leg's
/// approval and the pool's pull).
pub fn path_amount(
    amount: TokenAmount,
    pool_pays_fees: bool,
    path: PayoutPath,
    fee: TokenAmount,
) -> Option<TokenAmount> {
    if pool_pays_fees {
        return Some(amount);
    }
    let fees = match path {
        PayoutPath::Ckusdc => fee,
        PayoutPath::Icpswap { .. } => fee.checked_mul(2_u8)?,
    };
    amount
        .checked_sub(fees)
        .filter(|left| *left > TokenAmount::ZERO)
}

/// What an ICPSwap leg took from the canister's ckUSDC account so far: every approval's fee,
/// and once begun, the swap's deposit (its amount in, `swap_in`, and the pool's pull's fee),
/// or what the deposit cost in the end once undone. A leg from before approvals were counted
/// gave one once begun.
fn leg_cost(swap: &SwapLeg, swap_in: Option<TokenAmount>, fee: TokenAmount) -> Option<TokenAmount> {
    let approvals = swap.approvals.unwrap_or(u32::from(swap.begun));
    let approved = fee.checked_mul(approvals)?;
    if !swap.begun {
        return Some(approved);
    }
    let deposit = match swap.undone {
        Some(cost) => cost,
        None => swap_in?.checked_add(fee)?,
    };
    approved.checked_add(deposit)
}

/// A payout's progress as the log has it now: a deposit's, or a withdrawal's.
fn progress_of(id: PoolRef) -> Option<PayoutProgress> {
    if let Some(deposit) = pool_store::deposit(&id) {
        return Some(deposit.payout);
    }
    match pool_store::ledger()
        .pending
        .get(&PoolOpId::new(id.into_bytes()))
    {
        Some(PendingOp::Withdraw { payout, .. }) => Some(payout.clone()),
        _ => None,
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
    clock: impl Fn() -> Timestamp + Copy,
    guard: &impl Fn() -> Result<(), String>,
) -> Result<Done, Stop> {
    let attempt = payout
        .progress
        .attempt
        .ok_or_else(|| Stop::Failed("the payout has no dedup pair".to_string()))?;
    // the ledger the payout started with, whatever the settings say now
    let ckusdc = attempt
        .token
        .map_or(settings.ckusdc_ledger, |token| token.0);
    let fee_ck = settings
        .fee_of(ckusdc)
        .ok_or_else(|| Stop::Failed("no ckUSDC fee was read".to_string()))?;
    let first = match attempt.amount {
        Some(first) => first,
        // an attempt from before its amount was recorded: worked out as it was then
        None => path_amount(payout.amount, payout.pool_pays_fees, attempt.path, fee_ck)
            .ok_or_else(|| Stop::Failed("the amount does not cover the fees".to_string()))?,
    };
    let pool = match attempt.path {
        PayoutPath::Ckusdc => {
            let spent = leg_cost(&payout.progress.swap, None, fee_ck)
                .and_then(|cost| first.checked_add(fee_ck)?.checked_add(cost))
                .ok_or_else(|| Stop::Failed("the payout's cost is unknown".to_string()))?;
            open(guard)?;
            let block = ledgers
                .transfer(ckusdc, payout.to, first, payout.dedup(&attempt))
                .await
                .map_err(|error| call_stop("the ckUSDC transfer", error))?;
            return Ok(Done {
                token: ckusdc,
                paid: first,
                block,
                spent,
            });
        }
        PayoutPath::Icpswap { pool } => pool,
    };
    let swap_in = first;
    let (out, send) = match (payout.progress.swap.out, payout.progress.swap.send) {
        (Some(out), Some(send)) => (out, send),
        // an output from before its transfer was recorded: worked out as it was then
        (Some(out), None) => {
            let fee_out = settings
                .fee_of(payout.token_out)
                .ok_or_else(|| Stop::Failed(format!("no fee was read for {}", payout.token_out)))?;
            (out, out.checked_sub(fee_out).unwrap_or(TokenAmount::ZERO))
        }
        (None, _) => {
            let Some(_leg) = Leg::take(pool) else {
                return Err(Stop::Retry(format!(
                    "another payout's leg is at the ICPSwap pool {pool}"
                )));
            };
            swap_leg(
                ledgers,
                settings,
                &payout,
                LegAt {
                    ckusdc,
                    fee_ck,
                    pool,
                    swap_in,
                },
                clock,
                guard,
            )
            .await?
        }
    };
    if send == TokenAmount::ZERO {
        return Err(Stop::Failed(format!(
            "the output {out} does not cover its fees"
        )));
    }
    // read again: the approvals this drive gave are in it
    let swap = progress_of(payout.id).map_or(payout.progress.swap, |progress| progress.swap);
    let spent = leg_cost(&swap, Some(swap_in), fee_ck)
        .ok_or_else(|| Stop::Failed("the payout's cost is unknown".to_string()))?;
    open(guard)?;
    let block = ledgers
        .transfer(payout.token_out, payout.to, send, payout.dedup(&attempt))
        .await
        .map_err(|error| call_stop("the output's transfer", error))?;
    Ok(Done {
        token: payout.token_out,
        paid: send,
        block,
        spent,
    })
}

/// What an ICPSwap leg works with, fixed when its payout started.
#[derive(Clone, Copy)]
struct LegAt {
    ckusdc: Principal,
    fee_ck: TokenAmount,
    pool: Principal,
    swap_in: TokenAmount,
}

/// Records the fall back of a payout's leg to ckUSDC, with the exact transfer that pays all
/// of it, read on the payout as the log has it now (the approvals and the undo included).
fn fall_back(
    payout: &Payout,
    leg: LegAt,
    why: String,
    now: Timestamp,
) -> Result<(TokenAmount, TokenAmount), Stop> {
    let swap = progress_of(payout.id).map_or(payout.progress.swap, |progress| progress.swap);
    let amount = payout
        .fallback_amount(&swap, leg.fee_ck)
        .ok_or_else(|| Stop::Failed("the amount does not cover the fees".to_string()))?;
    record(
        PoolStep::FellBack {
            r#ref: payout.id,
            why,
            amount: Some(amount),
            token: Some(PrincipalBytes(leg.ckusdc)),
        },
        now,
    )?;
    Err(Stop::Again)
}

/// The ICPSwap leg up to its output, with the pool held for this payout alone: answers the
/// output and its transfer to the recipient, or why it stopped (`Stop::Again` once a fall
/// back to ckUSDC was recorded).
async fn swap_leg(
    ledgers: &impl Ledgers,
    settings: &PoolSettings,
    payout: &Payout,
    leg: LegAt,
    clock: impl Fn() -> Timestamp + Copy,
    guard: &impl Fn() -> Result<(), String>,
) -> Result<(TokenAmount, TokenAmount), Stop> {
    let LegAt {
        ckusdc,
        fee_ck,
        pool,
        swap_in,
    } = leg;
    if payout.progress.swap.begun {
        // undone already, its fall back not recorded yet
        if payout.progress.swap.undone.is_some() {
            return fall_back(payout, leg, "the swap did not happen".to_string(), clock());
        }
        return settle(ledgers, settings, payout, leg, None, clock).await;
    }
    // before the swap: a token no longer listed with this pool, or with no fee read, falls
    // back (the token's output could not be paid)
    let fee_out = match settings.fee_of(payout.token_out) {
        Some(fee) if settings.pool_of(payout.token_out) == Some(pool) => fee,
        _ => {
            return fall_back(
                payout,
                leg,
                format!(
                    "{} is no longer listed with the pool {pool}",
                    payout.token_out
                ),
                clock(),
            )
        }
    };
    // the recipient gets at least `min_out` after the pool's withdrawal to the canister and
    // the canister's transfer, each of which takes the token's fee
    let least = fee_out
        .checked_mul(2_u8)
        .and_then(|fees| TokenAmount::from(payout.min_out).checked_add(fees))
        .ok_or_else(|| Stop::Failed("min_out is too large".to_string()))?;
    let zero_for_one = match ledgers
        .direction(pool, ckusdc, payout.token_out)
        .await
        .map_err(|error| call_stop("the pool's metadata", error))?
    {
        Some(direction) => direction,
        None => {
            return fall_back(
                payout,
                leg,
                "the pool does not trade the pair".to_string(),
                clock(),
            )
        }
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
        return fall_back(payout, leg, why, clock());
    }
    let allowance = swap_in
        .checked_add(fee_ck)
        .ok_or_else(|| Stop::Failed("the allowance overflows".to_string()))?;
    open(guard)?;
    ledgers
        .approve(ckusdc, pool, allowance)
        .await
        .map_err(|error| call_stop("the approval", error))?;
    // the approval's fee is the payout's, whatever comes next
    record(PoolStep::SwapApproved { r#ref: payout.id }, clock())?;
    open(guard)?;
    record(PoolStep::SwapBegun { r#ref: payout.id }, clock())?;
    // begun: whatever this answers, the swap is never called again
    match ledgers
        .swap(pool, swap_in, zero_for_one, least, fee_ck, fee_out)
        .await
    {
        Ok(out) => {
            let send = output_send(out, fee_out);
            record(
                PoolStep::Swapped {
                    r#ref: payout.id,
                    out,
                    send: Some(send),
                },
                clock(),
            )?;
            Ok((out, send))
        }
        Err(CallError::Refused(why)) => {
            settle(ledgers, settings, payout, leg, Some(why), clock).await
        }
        Err(_) => settle(ledgers, settings, payout, leg, None, clock).await,
    }
}

/// The output's transfer to the recipient: what the pool answered, less the token's fee for
/// the pool's withdrawal to the canister and for the canister's transfer.
fn output_send(out: TokenAmount, fee_out: TokenAmount) -> TokenAmount {
    fee_out
        .checked_mul(2_u8)
        .and_then(|fees| out.checked_sub(fees))
        .unwrap_or(TokenAmount::ZERO)
}

/// A begun swap that did not answer with its output: what the pool holds of the canister's
/// tells what happened. The token there: the swap happened and its output stayed at the
/// pool; withdrawn, and paid. ckUSDC there (all of the amount in, or any of it after the
/// pool's own refusal): the swap did not happen; withdrawn back, and all of it paid as
/// ckUSDC. Nothing there after the pool's own refusal (`refused`): the deposit never
/// happened, all of it paid as ckUSDC. Anything else: the outcome is unknown, for a human.
/// ICPSwap's unused balances and its `withdraw` are not exercised against a live pool in this
/// build.
async fn settle(
    ledgers: &impl Ledgers,
    settings: &PoolSettings,
    payout: &Payout,
    leg: LegAt,
    refused: Option<String>,
    clock: impl Fn() -> Timestamp + Copy,
) -> Result<(TokenAmount, TokenAmount), Stop> {
    let LegAt {
        ckusdc,
        fee_ck,
        pool,
        swap_in,
    } = leg;
    let unknown = || Stop::Failed("the ICPSwap swap's outcome is unknown".to_string());
    let zero_for_one = ledgers
        .direction(pool, ckusdc, payout.token_out)
        .await
        .map_err(|error| call_stop("the pool's metadata", error))?
        .ok_or_else(unknown)?;
    let (unused0, unused1) = ledgers
        .unused(pool)
        .await
        .map_err(|error| call_stop("the pool's unused balances", error))?;
    let (ck_left, token_left) = if zero_for_one {
        (unused0, unused1)
    } else {
        (unused1, unused0)
    };
    // withdrawing from the pool brings money in: no guard
    if token_left > TokenAmount::ZERO {
        let fee_out = settings
            .fee_of(payout.token_out)
            .ok_or_else(|| Stop::Failed(format!("no fee was read for {}", payout.token_out)))?;
        ledgers
            .withdraw(pool, payout.token_out, token_left, fee_out)
            .await
            .map_err(|error| call_stop("the output's withdrawal from the pool", error))?;
        let send = output_send(token_left, fee_out);
        record(
            PoolStep::Swapped {
                r#ref: payout.id,
                out: token_left,
                send: Some(send),
            },
            clock(),
        )?;
        return Ok((token_left, send));
    }
    let deposit = swap_in
        .checked_add(fee_ck)
        .ok_or_else(|| Stop::Failed("the deposit overflows".to_string()))?;
    if ck_left >= swap_in || (refused.is_some() && ck_left > TokenAmount::ZERO) {
        // this leg's own amount at most: the pool holds nothing else of this payout's
        let back = ck_left.min(swap_in);
        ledgers
            .withdraw(pool, ckusdc, back, fee_ck)
            .await
            .map_err(|error| call_stop("the ckUSDC's withdrawal from the pool", error))?;
        // the withdrawal's transfer takes the ledger's fee out of what comes back
        let returned = back.checked_sub(fee_ck).unwrap_or(TokenAmount::ZERO);
        let cost = deposit.checked_sub(returned).unwrap_or(TokenAmount::ZERO);
        record(
            PoolStep::SwapUndone {
                r#ref: payout.id,
                cost,
            },
            clock(),
        )?;
        return fall_back(
            payout,
            leg,
            format!("the swap did not happen; {returned} came back from the pool"),
            clock(),
        );
    }
    match refused {
        Some(why) => {
            record(
                PoolStep::SwapUndone {
                    r#ref: payout.id,
                    cost: TokenAmount::ZERO,
                },
                clock(),
            )?;
            fall_back(
                payout,
                leg,
                format!("the pool refused the swap before its deposit: {why}"),
                clock(),
            )
        }
        None => Err(unknown()),
    }
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
            .ok_or_else(|| Stop::Retry("the pool has no settings".to_string()))?;
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
                            // the pool pays the fees on top: the path moves all of it
                            amount: Some(deposit.owed()),
                            token: Some(PrincipalBytes(settings.ckusdc_ledger)),
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

/// One pass over everything the pool owes (`drive_with`).
pub async fn drive(ledgers: &impl Ledgers, clock: impl Fn() -> Timestamp + Copy) -> usize {
    drive_with(ledgers, clock, &guard).await
}

thread_local! {
    // Heap by necessity, and only for the log: the queue's head already alerted on, so a
    // head that waits raises one line and not one per drive. An upgrade may repeat it once.
    static ALERTED_HEAD: std::cell::Cell<Option<PoolRef>> = const { std::cell::Cell::new(None) };
}

/// One line for ops when the queue's head has waited longer than one books interval for
/// the ICP pocket to cover it: what P11c's urgent rebalance is for.
fn alert_if_waited(now: Timestamp) {
    let ledger = pool_store::ledger();
    let (Some((head, _)), Some(since), Some(need)) =
        (ledger.queue.head(), ledger.head_since, ledger.queue_need())
    else {
        return;
    };
    let interval = crate::storage::config::get().books_check_interval.0;
    if now.saturating_duration_since(since) <= interval
        || ALERTED_HEAD.with(|alerted| alerted.get()) == Some(head)
    {
        return;
    }
    ALERTED_HEAD.with(|alerted| alerted.set(Some(head)));
    crate::pool::log(&format!(
        "ALERT: the pool payout {head} has waited {:?} for the ICP pocket, which is {need} short",
        now.saturating_duration_since(since)
    ));
}

/// One pass over everything the pool owes. What moves nothing out runs whatever the pool's
/// guard says: an ICP stake's pull asked again, an arrived stake's mint. Then, while the pool
/// is open: the payouts already started, the queued ones first in first out (stopping at the
/// first the ICP pocket does not cover, which waits and is alerted on), and the withdrawals.
/// A head that waits holds back only the payouts queued behind it.
pub async fn drive_with(
    ledgers: &impl Ledgers,
    clock: impl Fn() -> Timestamp + Copy,
    guard: &impl Fn() -> Result<(), String>,
) -> usize {
    let now = clock();
    let work = read_state(|state| {
        let ledger = state.store().pool_ledger();
        let mut work = Work::default();
        for (id, op) in &ledger.pending {
            match op {
                // a pull too old to deduplicate is a human's
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
    });
    let mut acted = 0;
    let mut tally = |stop: Option<Stop>, id: PoolRef, counts: bool| match stop {
        None | Some(Stop::Later) | Some(Stop::Again) | Some(Stop::Queued) => acted += 1,
        Some(Stop::Failed(why)) if counts => fail(id, why, clock()),
        Some(Stop::Failed(why)) | Some(Stop::Append(why)) | Some(Stop::Retry(why)) => {
            crate::pool::log(&format!("the pool's {id} waits: {why}"))
        }
    };
    for id in work.pulls {
        let Some(_busy) = Busy::take(id.into_bytes()) else {
            continue;
        };
        tally(
            pull_stake(ledgers, id, clock).await.err(),
            payout_ref(id),
            false,
        );
    }
    for r#ref in work.mints {
        let Some(_busy) = Busy::take(r#ref.into_bytes()) else {
            continue;
        };
        tally(
            drive_deposit(ledgers, r#ref, clock, guard).await.err(),
            r#ref,
            true,
        );
    }
    if guard().is_err() {
        return acted;
    }
    for r#ref in work.started {
        let Some(_busy) = Busy::take(r#ref.into_bytes()) else {
            continue;
        };
        tally(
            drive_deposit(ledgers, r#ref, clock, guard).await.err(),
            r#ref,
            true,
        );
    }
    for r#ref in work.queue {
        if guard().is_err() {
            return acted;
        }
        let Some(_busy) = Busy::take(r#ref.into_bytes()) else {
            break;
        };
        match drive_deposit(ledgers, r#ref, clock, guard).await {
            // first in first out: nothing behind a queued payout starts ahead of it
            Err(Stop::Queued) => {
                alert_if_waited(now);
                break;
            }
            other => tally(other.err(), r#ref, true),
        }
    }
    for id in work.withdrawals {
        if guard().is_err() {
            return acted;
        }
        let Some(_busy) = Busy::take(id.into_bytes()) else {
            continue;
        };
        tally(
            drive_withdrawal(ledgers, id, clock, guard).await.err(),
            payout_ref(id),
            true,
        );
    }
    acted
}

/// What one drive works through, read once at its start.
#[derive(Default)]
struct Work {
    pulls: Vec<PoolOpId>,
    mints: Vec<PoolRef>,
    started: Vec<PoolRef>,
    queue: Vec<PoolRef>,
    withdrawals: Vec<PoolOpId>,
}
