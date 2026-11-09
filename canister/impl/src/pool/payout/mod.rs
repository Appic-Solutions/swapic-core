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
//!   holds of the canister's ABOVE what it held just before the swap (read while this leg
//!   alone was at the pool, recorded with `SwapBegun`) tells what happened, and only that is
//!   this leg's. Its output there is withdrawn and paid; its ckUSDC there is withdrawn back,
//!   never more than it deposited (`SwapUndone`, the amount recorded), and all of it paid as
//!   ckUSDC; nothing there after the pool refused the swap means the deposit never happened,
//!   and all of it is paid as ckUSDC. Only nothing there after an answer that never came, or
//!   a leg from before the pool's holdings were read, stops for a human. While a driven leg
//!   at a pool is begun and not settled, no other leg begins there.
//! - No path swaps with a minimum below `min_out` plus the token's fee twice, and no output
//!   transfer pays the recipient below `min_out`: a swap that cannot meet it falls back.
//! - `Paid` (or `WithdrawnIcp`), with the ckUSDC that left the pocket, every fee included.
//!
//! A refusal (or an outcome the ledger can no longer tell) is `AttemptFailed`, the payout
//! stays where it is and is driven again; at the tenth it stops for a human
//! (`PayoutFailed`). A call whose outcome is unknown records nothing and is asked again, the
//! same call: a ledger's passing trouble never stops a payout. The pool's guard runs before
//! every outflow call, after every await: a pool closed meanwhile pays nothing more.

#[cfg(test)]
mod tests;

mod drive;
pub use drive::*;

use crate::ic::{self, Account, CallError, Dedup};
use crate::pool::legs::{self, LegEnd, LegSpec};
use crate::state::{State, Store};
use crate::storage::events::read_state;
use crate::storage::pool as pool_store;
use candid::{Nat, Principal};
use std::cell::RefCell;
use std::collections::BTreeSet;
use types::hash::{PoolOpId, PoolRef};
use types::pool::{
    evm_payout_ref, payout_ref, DepositKind, DepositStatus, IcpAccount, PayoutAttempt, PayoutPath,
    PayoutProgress, PendingOp, PoolHeld, PoolSettings, PoolStep, PrincipalBytes, Registered,
    RoutePath, SwapLeg, LEG_BACK, LEG_SECOND,
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

    /// Whether a drive or a door holds the payout `r#ref` names now.
    pub fn is_held(r#ref: &PoolRef) -> bool {
        BUSY.with(|busy| busy.borrow().contains(&r#ref.into_bytes()))
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        BUSY.with(|busy| busy.borrow_mut().remove(&self.0));
    }
}

/// Carries payout `id` on in a message of its own, holding it, so the door that opened it
/// answers its caller as soon as the money is in: a wallet waits for that answer, and the sale
/// and the signature behind it take half a minute. A trap in `work` takes back nothing the door
/// wrote. Whatever stops `work`, the payout timer carries the payout on, as it does one somebody
/// else holds when this is called. An upgrade before the message runs drops it and the hold
/// together, and the timer carries the payout on from there too.
pub(crate) fn carry_on<F>(id: u64, work: impl FnOnce(Busy) -> F + 'static)
where
    F: std::future::Future<Output = ()> + 'static,
{
    let Some(busy) = Busy::take(evm_payout_ref(id).into_bytes()) else {
        return;
    };
    // unit tests run off the canister, where there is no timer to set
    #[cfg(target_arch = "wasm32")]
    ic_cdk_timers::set_timer(std::time::Duration::ZERO, move || ic_cdk::spawn(work(busy)));
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (busy, work);
}

thread_local! {
    // Heap by necessity: the ICPSwap pools a payout's leg is at right now. Two legs at one
    // pool would overwrite each other's approval and mix their unused balances there.
    static LEGS: RefCell<BTreeSet<Principal>> = const { RefCell::new(BTreeSet::new()) };
}

/// Held while one payout's leg is at an ICPSwap pool, released on drop, a trap included.
pub(crate) struct Leg(Principal);

impl Leg {
    pub(crate) fn take(pool: Principal) -> Option<Self> {
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
        // the tokens read when the routes were set; the pool itself only for one not read then
        let stored = pool_store::ledger()
            .settings
            .and_then(|settings| settings.routes)
            .and_then(|routes| routes.tokens_of(pool));
        if let Some(tokens) = stored {
            return Ok(tokens.direction(token_in, token_out));
        }
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

/// One payout as the driver works it: a swap deposit's or a withdrawal's, whose own amount
/// pays every ledger fee of its path (the user pays every fee, the pool
/// none).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Payout {
    pub id: PoolRef,
    pub to: IcpAccount,
    pub token_out: Principal,
    pub min_out: u128,
    /// A deposit: `arrived - premium`, its payout and its ICP fees. A withdrawal: the ckUSDC
    /// the shares were worth. Every fee comes out of it.
    pub amount: TokenAmount,
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

impl From<crate::storage::events::AppendError> for Stop {
    fn from(error: crate::storage::events::AppendError) -> Self {
        Stop::Append(error.to_string())
    }
}

/// What a call's failure means for a payout: an outcome unknown is asked again as it is; a
/// refusal, and an outcome the ledger can no longer tell, count toward the stop.
pub(crate) fn call_stop(what: &str, error: CallError) -> Stop {
    match error {
        CallError::Unknown(_) => Stop::Retry(format!("{what}: {error}")),
        CallError::Refused(_) | CallError::Expired(_) => Stop::Failed(format!("{what}: {error}")),
    }
}

/// The pool's guard, read before every outflow call: a pool closed while a payout awaited
/// moves nothing more.
pub(crate) fn open(guard: &impl Fn() -> Result<(), String>) -> Result<(), Stop> {
    guard().map_err(|closed| Stop::Retry(format!("the pool is closed: {closed}")))
}

impl Payout {
    fn dedup(&self, attempt: &PayoutAttempt) -> Dedup {
        Dedup {
            memo: attempt.memo,
            created_at_nanos: attempt.created_at.as_nanos(),
        }
    }

    /// The ckUSDC transfer that pays all of a payout that fell back: its amount less the
    /// transfer's fee and what its legs cost in ckUSDC in the end (a way back from ICP brings
    /// some back), every fee the user's.
    fn fallback_amount(&self, progress: &PayoutProgress, fee: TokenAmount) -> Option<TokenAmount> {
        self.amount
            .checked_sub(fee)?
            .checked_add(back_gain(progress, fee))?
            .checked_sub(leg_cost(&progress.swap, None, fee)?)
            .filter(|sent| *sent > TokenAmount::ZERO)
    }
}

/// What a payout's path moves first, fixed when it starts and reused on every retry: the
/// ckUSDC transfer's amount, or the ICPSwap leg's amount in. `amount` pays the fees (the
/// transfer's, or the leg's approval and the pool's pull).
pub fn path_amount(amount: TokenAmount, path: PayoutPath, fee: TokenAmount) -> Option<TokenAmount> {
    let fees = match path {
        PayoutPath::Ckusdc => fee,
        // the first leg's approval and pull; a path through ICP's second leg pays in ICP
        PayoutPath::Icpswap { .. } | PayoutPath::IcpswapVia { .. } => fee.checked_mul(2_u8)?,
    };
    amount
        .checked_sub(fees)
        .filter(|left| *left > TokenAmount::ZERO)
}

/// What an ICPSwap leg took from the canister's ckUSDC account so far: every approval's fee,
/// and once begun, the swap's deposit (its amount in and the pool's pull's fee), or what the
/// deposit cost in the end once undone. The deposit is the one the leg recorded at its begin,
/// which an approval asked again made one fee less than the amount it was handed;
/// `swap_in` only for a leg from before that amount was recorded. A leg from before
/// approvals were counted gave one once begun.
fn leg_cost(swap: &SwapLeg, swap_in: Option<TokenAmount>, fee: TokenAmount) -> Option<TokenAmount> {
    let approvals = swap.approvals.unwrap_or(u32::from(swap.begun));
    let approved = fee.checked_mul(approvals)?;
    if !swap.begun {
        return Some(approved);
    }
    let deposit = match swap.undone {
        Some(cost) => cost,
        None => swap.amount_in.or(swap_in)?.checked_add(fee)?,
    };
    approved.checked_add(deposit)
}

/// The ckUSDC a path through ICP's way back brought into the canister's account: its output
/// less the pool's withdrawal fee. Nothing on any other path.
fn back_gain(progress: &PayoutProgress, fee: TokenAmount) -> TokenAmount {
    match progress
        .back
        .and_then(|back| back.out.map(|out| (out, back.send)))
    {
        Some((_, Some(landed))) => landed,
        Some((out, None)) => out.checked_sub(fee).unwrap_or(TokenAmount::ZERO),
        None => TokenAmount::ZERO,
    }
}

/// A payout's progress as the log has it now: a deposit's, or a withdrawal's.
pub(crate) fn progress_of(id: PoolRef) -> Option<PayoutProgress> {
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
        progress: payout.clone(),
    })
}

/// The path a payout of `token_out` takes: ckUSDC for ckUSDC, its route's leg or legs for a
/// listed token (directly, or through ICP), and ckUSDC for a token no longer listed.
pub fn path_for(settings: &PoolSettings, token_out: Principal) -> PayoutPath {
    match settings.path_of(token_out) {
        Some(RoutePath::Direct { pool }) => PayoutPath::Icpswap { pool },
        Some(RoutePath::ViaIcp { first, second, icp }) => {
            PayoutPath::IcpswapVia { first, second, icp }
        }
        Some(RoutePath::Ckusdc) | None => PayoutPath::Ckusdc,
    }
}

/// Records `step` at `now`; a line not appended stops the drive, driven again.
pub(crate) fn record(step: PoolStep, now: Timestamp) -> Result<(), Stop> {
    pool_store::record_at(step, now)?;
    Ok(())
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
        None => path_amount(payout.amount, attempt.path, fee_ck)
            .ok_or_else(|| Stop::Failed("the amount does not cover the fees".to_string()))?,
    };
    let pool = match attempt.path {
        PayoutPath::Ckusdc => {
            // what left the ICP pocket: the transfer and its fee, what the legs cost, less
            // what a way back from ICP brought back
            let spent = leg_cost(&payout.progress.swap, None, fee_ck)
                .and_then(|cost| first.checked_add(fee_ck)?.checked_add(cost))
                .map(|spent| {
                    spent
                        .checked_sub(back_gain(&payout.progress, fee_ck))
                        .unwrap_or(TokenAmount::ZERO)
                })
                .ok_or_else(|| Stop::Failed("the payout's cost is unknown".to_string()))?;
            open(guard)?;
            let block = send_out(
                ledgers,
                &payout,
                &attempt,
                ckusdc,
                first,
                spent,
                "the ckUSDC transfer",
                clock,
            )
            .await?;
            return Ok(Done {
                token: ckusdc,
                paid: first,
                block,
                spent,
            });
        }
        PayoutPath::Icpswap { pool } => pool,
        PayoutPath::IcpswapVia {
            first: pool,
            second,
            icp,
        } => {
            let via = ViaAt {
                ckusdc,
                fee_ck,
                pool,
                second,
                icp,
                swap_in: first,
            };
            return via_icp(ledgers, settings, &payout, attempt, via, clock, guard).await;
        }
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
    // never below `min_out`: a swap that could not meet it fell back before it began, so
    // an output below it is the pool's answer breaking its own minimum, for a human
    if send == TokenAmount::ZERO || send < TokenAmount::from(payout.min_out) {
        return Err(Stop::Failed(format!(
            "the output {out} would pay {send}, below the {} the recipient asked",
            payout.min_out
        )));
    }
    // read again: the approvals this drive gave are in it
    let swap = progress_of(payout.id).map_or(payout.progress.swap, |progress| progress.swap);
    let spent = leg_cost(&swap, Some(swap_in), fee_ck)
        .ok_or_else(|| Stop::Failed("the payout's cost is unknown".to_string()))?;
    open(guard)?;
    let block = send_out(
        ledgers,
        &payout,
        &attempt,
        payout.token_out,
        send,
        spent,
        "the output's transfer",
        clock,
    )
    .await?;
    Ok(Done {
        token: payout.token_out,
        paid: send,
        block,
        spent,
    })
}

/// Whether `token`'s ledger is one the canister knows to deduplicate a payout's transfer on
/// its memo and `created_at_time` within the window: the ckUSDC ledger the payout started
/// with, or the ICP ledger the routes name.
fn deduplicates_on(token: Principal, attempt: &PayoutAttempt) -> bool {
    let settings = pool_store::ledger().settings;
    let started_on = attempt
        .token
        .map(|started| started.0)
        .or(settings.as_ref().map(|settings| settings.ckusdc_ledger));
    let icp = settings
        .and_then(|settings| settings.routes)
        .and_then(|routes| routes.icp_ledger)
        .map(|icp| icp.0);
    Some(token) == started_on || Some(token) == icp
}

/// Holds a payout whose transfer on a ledger that may not deduplicate it has no known answer
/// for a human, at once: never asked again by the drive; `MarkPaid` if it landed.
fn hold_unanswered(payout: &Payout, what: &str, now: Timestamp) -> Stop {
    let why = format!(
        "ALERT: {what} on a ledger the canister does not know to deduplicate has no known \
         answer: held for a human, never asked again (MarkPaid if it landed)"
    );
    match pool_store::record_at(
        PoolStep::TransferLost {
            r#ref: payout.id,
            held: Some(why.clone()),
        },
        now,
    ) {
        Ok(_) => Stop::Retry(why),
        Err(error) => Stop::Append(format!("{why}; not noted: {error}")),
    }
}

/// The payout's transfer of `amount` of `token` to its recipient, its ask recorded before the
/// call: from then on it may have landed, until the ledger refuses
/// it for sure, which is recorded too. A controller refunds a payout only when nothing of it
/// may have landed, and marks it paid on the block of exactly this transfer.
#[allow(clippy::too_many_arguments)]
async fn send_out(
    ledgers: &impl Ledgers,
    payout: &Payout,
    attempt: &PayoutAttempt,
    token: Principal,
    amount: TokenAmount,
    spent: TokenAmount,
    what: &str,
    clock: impl Fn() -> Timestamp,
) -> Result<u64, Stop> {
    let token_bytes = PrincipalBytes(token);
    let sent = progress_of(payout.id).and_then(|progress| progress.sent);
    // asked already, on these terms and not refused since: its answer not known
    let asked = sent
        .is_some_and(|sent| !sent.refused && (sent.token, sent.amount) == (token_bytes, amount));
    // an answer of it ever lost: an ask keeps the mark, and only this drive, which holds the
    // payout, notes one
    let lost = sent.is_some_and(|sent| sent.lost.0);
    // the ledgers the canister knows to deduplicate a transfer on its memo and
    // `created_at_time` within the window: the ckUSDC ledger the payout started with, and
    // the ICP ledger the routes name. Any other may take a re-ask as a new transfer, so a
    // transfer there whose answer is not known is never asked again: held for a human
    let deduplicates = deduplicates_on(token, attempt);
    if asked && !deduplicates {
        return Err(hold_unanswered(payout, what, clock()));
    }
    // an ask a rule of the fold refuses (an earlier one on other terms may have landed, or
    // any other) is a counted failure, so the payout reaches a human; only the log's own
    // trouble (a divergence, an index, storage) is driven again, uncounted
    if !asked {
        pool_store::record_at(
            PoolStep::TransferAsked {
                r#ref: payout.id,
                token: token_bytes,
                amount,
                spent,
            },
            clock(),
        )
        .map_err(|error| match error {
            crate::storage::events::AppendError::Transition(_) => {
                Stop::Failed(format!("{what} was not asked: {error}"))
            }
            error => Stop::from(error),
        })?;
    }
    match ledgers
        .transfer(token, payout.to, amount, payout.dedup(attempt))
        .await
    {
        Ok(block) => Ok(block),
        Err(error) => {
            // an answer lost (unknown, or too old to tell) is kept for good: it may have
            // landed whatever comes after, and noted before anything
            // else: a lost answer not noted stops the drive, uncounted, so no later refusal
            // can make it look safe
            if matches!(error, CallError::Unknown(_) | CallError::Expired(_)) && !deduplicates {
                return Err(hold_unanswered(payout, what, clock()));
            }
            if matches!(error, CallError::Unknown(_) | CallError::Expired(_)) && !lost {
                pool_store::record_at(
                    PoolStep::TransferLost {
                        r#ref: payout.id,
                        held: None,
                    },
                    clock(),
                )
                .map_err(|unnoted| {
                    Stop::Append(format!("{what}'s lost answer was not noted: {unnoted}"))
                })?;
            }
            // a refusal counts whatever its line does: one not recorded leaves the transfer
            // asked, which only blocks a refund (the safe way)
            if matches!(error, CallError::Refused(_)) {
                if let Err(refused) =
                    pool_store::record_at(PoolStep::TransferRefused { r#ref: payout.id }, clock())
                {
                    crate::pool::log(&format!(
                        "the pool payout {}'s refused transfer was not noted: {refused}",
                        payout.id
                    ));
                }
            }
            Err(call_stop(what, error))
        }
    }
}

/// What an ICPSwap leg works with, fixed when its payout started.
#[derive(Clone, Copy)]
struct LegAt {
    ckusdc: Principal,
    fee_ck: TokenAmount,
    pool: Principal,
    swap_in: TokenAmount,
}

/// What a path through ICP works with, fixed when its payout started.
#[derive(Clone, Copy)]
struct ViaAt {
    ckusdc: Principal,
    fee_ck: TokenAmount,
    /// The ICP/ckUSDC pool: the first leg and the way back.
    pool: Principal,
    /// The token/ICP pool: the second leg.
    second: Principal,
    icp: Principal,
    swap_in: TokenAmount,
}

impl ViaAt {
    fn first_leg(&self) -> LegAt {
        LegAt {
            ckusdc: self.ckusdc,
            fee_ck: self.fee_ck,
            pool: self.pool,
            swap_in: self.swap_in,
        }
    }
}

/// The ICP fees a path through ICP spends in the canister's ICP account before its second
/// leg sells: the first leg's withdrawal to the canister, the second leg's approval and its
/// pull.
const ICP_FEES_BEFORE_SECOND: u8 = 3;

/// A payout through ICP: ckUSDC into ICP on the ICP/ckUSDC pool, the ICP into the token out on
/// its ICP pool, the token's transfer. Before the first leg both pools' quotes are read: the
/// second leg's must reach the recipient's minimum (below it, declined once, then all of it as
/// ckUSDC), and the first leg's minimum is what the second needs. A first leg that did not
/// happen falls back as before. A second leg that cannot pay (its quote short twice, the
/// pool not trading the pair, its swap not happening) swaps the ICP back to ckUSDC, and all of
/// it is paid as ckUSDC; a way back that fails too stops for a human. Each leg holds its pool.
#[allow(clippy::too_many_arguments)]
async fn via_icp(
    ledgers: &impl Ledgers,
    settings: &PoolSettings,
    payout: &Payout,
    attempt: PayoutAttempt,
    via: ViaAt,
    clock: impl Fn() -> Timestamp + Copy,
    guard: &impl Fn() -> Result<(), String>,
) -> Result<Done, Stop> {
    let leg0 = via.first_leg();
    let fee_icp = settings.fee_of(via.icp);
    let fee_out = settings.fee_of(payout.token_out);
    let progress = &payout.progress;
    let least = fee_out
        .and_then(|fee| fee.checked_mul(2_u8))
        .and_then(|fees| TokenAmount::from(payout.min_out).checked_add(fees));
    let out0 = match progress.swap.out {
        Some(out) => out,
        None => {
            let Some(_held) = Leg::take(via.pool) else {
                return Err(Stop::Retry(format!(
                    "another payout's leg is at the ICPSwap pool {}",
                    via.pool
                )));
            };
            let still_routed = settings.path_of(payout.token_out)
                == Some(RoutePath::ViaIcp {
                    first: via.pool,
                    second: via.second,
                    icp: via.icp,
                });
            let (fee_icp, least) = match (fee_icp, least) {
                (Some(fee_icp), Some(least)) if still_routed || progress.swap.begun => {
                    (fee_icp, least)
                }
                _ if !progress.swap.begun => {
                    return via_fall_back(
                        payout,
                        leg0,
                        format!(
                            "{} is no longer routed through ICP with these pools",
                            payout.token_out
                        ),
                        clock(),
                    )
                }
                _ => {
                    return Err(Stop::Failed(
                        "no fee was read for ICP or the token".to_string(),
                    ))
                }
            };
            if let Some(other) = read_state(|state| unsettled_at(state, via.pool, payout.id)) {
                return Err(Stop::Retry(format!(
                    "the swap of {other} at the ICPSwap pool {} is not settled yet",
                    via.pool
                )));
            }
            let mut least0 = TokenAmount::ZERO;
            if !progress.swap.begun {
                let fees = fee_icp
                    .checked_mul(ICP_FEES_BEFORE_SECOND)
                    .ok_or_else(|| Stop::Failed("the ICP fees overflow".to_string()))?;
                let chained = legs::chained_least(
                    ledgers,
                    (via.pool, via.ckusdc, via.swap_in),
                    (via.second, via.icp, payout.token_out),
                    fees,
                    least,
                    legs::ChainNames {
                        first: "the ICP/ckUSDC pool",
                        second: "the token's ICP pool",
                        need: "needed",
                    },
                )
                .await?;
                match chained {
                    Ok(at_least) => least0 = at_least,
                    Err(why) => {
                        if progress.swap.declined == 0 {
                            record(
                                PoolStep::SwapDeclined {
                                    r#ref: payout.id,
                                    why,
                                    leg: None,
                                },
                                clock(),
                            )?;
                            return Err(Stop::Later);
                        }
                        return via_fall_back(payout, leg0, why, clock());
                    }
                }
            }
            let spec = LegSpec {
                id: payout.id,
                leg: None,
                pool: via.pool,
                token_in: via.ckusdc,
                token_out: via.icp,
                amount_in: via.swap_in,
                least: least0,
                fee_in: via.fee_ck,
                fee_out: fee_icp,
                pays_out: false,
            };
            match legs::run(ledgers, spec, progress.swap, clock, guard).await? {
                LegEnd::Out(out) => out,
                LegEnd::Short(quoted) => {
                    return via_fall_back(
                        payout,
                        leg0,
                        format!("the ICP/ckUSDC pool quotes {quoted}, below the {least0} needed"),
                        clock(),
                    )
                }
                LegEnd::NoPair => {
                    return via_fall_back(
                        payout,
                        leg0,
                        "the ICP/ckUSDC pool does not trade the pair".to_string(),
                        clock(),
                    )
                }
                LegEnd::NotDone(why) => return via_fall_back(payout, leg0, why, clock()),
            }
        }
    };
    let fee_icp = fee_icp.ok_or_else(|| Stop::Failed("no fee was read for ICP".to_string()))?;
    // read again: the first leg's lines this drive recorded are in it
    let progress = progress_of(payout.id).unwrap_or_else(|| payout.progress.clone());
    let second = progress.leg(Some(LEG_SECOND));
    let back = progress.leg(Some(LEG_BACK));
    let in1 = out0
        .checked_sub(
            fee_icp
                .checked_mul(ICP_FEES_BEFORE_SECOND)
                .unwrap_or(TokenAmount::MAX),
        )
        .unwrap_or(TokenAmount::ZERO);
    let paid_by_second = if back.begun || back.out.is_some() || in1 == TokenAmount::ZERO {
        None
    } else {
        match (second.out, fee_out, least) {
            (Some(out), Some(fee_out), _) => Some((out, second.send, fee_out)),
            (None, Some(fee_out), Some(least)) => {
                let Some(_held) = Leg::take(via.second) else {
                    return Err(Stop::Retry(format!(
                        "another payout's leg is at the ICPSwap pool {}",
                        via.second
                    )));
                };
                if let Some(other) = read_state(|state| unsettled_at(state, via.second, payout.id))
                {
                    return Err(Stop::Retry(format!(
                        "the swap of {other} at the ICPSwap pool {} is not settled yet",
                        via.second
                    )));
                }
                let spec = LegSpec {
                    id: payout.id,
                    leg: Some(LEG_SECOND),
                    pool: via.second,
                    token_in: via.icp,
                    token_out: payout.token_out,
                    amount_in: in1,
                    least,
                    fee_in: fee_icp,
                    fee_out,
                    pays_out: true,
                };
                match legs::run(ledgers, spec, second, clock, guard).await? {
                    LegEnd::Out(out) => Some((out, Some(spec.send_of(out)), fee_out)),
                    LegEnd::Short(quoted) if second.declined == 0 => {
                        record(
                            PoolStep::SwapDeclined {
                                r#ref: payout.id,
                                why: format!(
                                    "the second leg's pool quotes {quoted}, below the {least} needed"
                                ),
                                leg: Some(LEG_SECOND),
                            },
                            clock(),
                        )?;
                        return Err(Stop::Later);
                    }
                    LegEnd::Short(_) | LegEnd::NoPair | LegEnd::NotDone(_) => None,
                }
            }
            // the token's fee is no longer known: its output could not be paid
            _ => None,
        }
    };
    if let Some((out, send, fee_out)) = paid_by_second {
        let send = send.unwrap_or_else(|| {
            out.checked_sub(fee_out.checked_mul(2_u8).unwrap_or(TokenAmount::MAX))
                .unwrap_or(TokenAmount::ZERO)
        });
        if send == TokenAmount::ZERO || send < TokenAmount::from(payout.min_out) {
            return Err(Stop::Failed(format!(
                "the output {out} would pay {send}, below the {} the recipient asked",
                payout.min_out
            )));
        }
        let spent = leg_cost(&progress.swap, Some(via.swap_in), via.fee_ck)
            .ok_or_else(|| Stop::Failed("the payout's cost is unknown".to_string()))?;
        open(guard)?;
        let block = send_out(
            ledgers,
            payout,
            &attempt,
            payout.token_out,
            send,
            spent,
            "the output's transfer",
            clock,
        )
        .await?;
        return Ok(Done {
            token: payout.token_out,
            paid: send,
            block,
            spent,
        });
    }
    // the second leg cannot pay: the ICP the canister holds goes back to ckUSDC, all of it
    let held = out0
        .checked_sub(fee_icp)
        .and_then(|left| left.checked_sub(legs::cost_in(&second, fee_icp)?))
        .unwrap_or(TokenAmount::ZERO);
    let amount_back = held
        .checked_sub(fee_icp.checked_mul(2_u8).unwrap_or(TokenAmount::MAX))
        .unwrap_or(TokenAmount::ZERO);
    if back.out.is_none() {
        let Some(_held) = Leg::take(via.pool) else {
            return Err(Stop::Retry(format!(
                "another payout's leg is at the ICPSwap pool {}",
                via.pool
            )));
        };
        if let Some(other) = read_state(|state| unsettled_at(state, via.pool, payout.id)) {
            return Err(Stop::Retry(format!(
                "the swap of {other} at the ICPSwap pool {} is not settled yet",
                via.pool
            )));
        }
        let spec = LegSpec {
            id: payout.id,
            leg: Some(LEG_BACK),
            pool: via.pool,
            token_in: via.icp,
            token_out: via.ckusdc,
            amount_in: amount_back,
            least: TokenAmount::ONE,
            fee_in: fee_icp,
            fee_out: via.fee_ck,
            pays_out: false,
        };
        if amount_back == TokenAmount::ZERO {
            return Err(Stop::Failed(
                "ALERT: the ICP the first leg bought does not cover the way back".to_string(),
            ));
        }
        match legs::run(ledgers, spec, back, clock, guard).await? {
            LegEnd::Out(_) => {}
            LegEnd::Short(_) | LegEnd::NoPair | LegEnd::NotDone(_) => return Err(Stop::Failed(
                "ALERT: the ICP could not be swapped back to ckUSDC; the payout waits for a human"
                    .to_string(),
            )),
        }
    }
    via_fall_back(
        payout,
        leg0,
        "the second leg could not pay; the ICP came back as ckUSDC".to_string(),
        clock(),
    )
}

/// [`fall_back`] for a path through ICP, which answers a payout or why it stopped.
fn via_fall_back(payout: &Payout, leg: LegAt, why: String, now: Timestamp) -> Result<Done, Stop> {
    fall_back(payout, leg, why, now)?;
    Err(Stop::Again)
}

/// Records the fall back of a payout's leg to ckUSDC, with the exact transfer that pays all
/// of it, read on the payout as the log has it now (the approvals and the undo included).
fn fall_back(
    payout: &Payout,
    leg: LegAt,
    why: String,
    now: Timestamp,
) -> Result<(TokenAmount, TokenAmount), Stop> {
    let progress = progress_of(payout.id).unwrap_or_else(|| payout.progress.clone());
    let amount = payout
        .fallback_amount(&progress, leg.fee_ck)
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
        let before = payout.progress.swap.before;
        // what it deposited is what its begin recorded
        let leg = LegAt {
            swap_in: payout.progress.swap.amount_in.unwrap_or(swap_in),
            ..leg
        };
        return settle(ledgers, settings, payout, leg, before, None, clock).await;
    }
    // an approval given before this run is one fee more: the user's, out of what the leg
    // deposits
    let swap_in =
        crate::pool::legs::after_approvals(swap_in, fee_ck, payout.progress.swap.approvals)
            .ok_or_else(|| {
                Stop::Failed("the approvals given take all the leg deposits".to_string())
            })?;
    let leg = LegAt { swap_in, ..leg };
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
    // what the pool holds of the canister's is known to be no one's but a stopped payout's
    // only while no other driven leg there is begun and unsettled
    if let Some(other) = read_state(|state| unsettled_at(state, pool, payout.id)) {
        return Err(Stop::Retry(format!(
            "the swap of {other} at the ICPSwap pool {pool} is not settled yet"
        )));
    }
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
                    leg: None,
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
    // what the pool holds of the canister's before this leg's deposit, read while this leg
    // alone is at the pool: only what is there above it is ever this leg's
    let before = held_at(ledgers, pool, zero_for_one).await?;
    open(guard)?;
    ledgers
        .approve(ckusdc, pool, allowance)
        .await
        .map_err(|error| call_stop("the approval", error))?;
    // the approval's fee is the payout's, whatever comes next
    record(
        PoolStep::SwapApproved {
            r#ref: payout.id,
            leg: None,
        },
        clock(),
    )?;
    open(guard)?;
    record(
        PoolStep::SwapBegun {
            r#ref: payout.id,
            before: Some(before),
            amount_in: Some(swap_in),
            leg: None,
        },
        clock(),
    )?;
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
                    leg: None,
                },
                clock(),
            )?;
            Ok((out, send))
        }
        // the read just made, not the payout as it was before `SwapBegun`
        Err(CallError::Refused(why)) => {
            settle(
                ledgers,
                settings,
                payout,
                leg,
                Some(before),
                Some(why),
                clock,
            )
            .await
        }
        Err(_) => settle(ledgers, settings, payout, leg, Some(before), None, clock).await,
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

/// What the pool holds of the canister's, unused, as ckUSDC and the leg's token.
async fn held_at(
    ledgers: &impl Ledgers,
    pool: Principal,
    zero_for_one: bool,
) -> Result<PoolHeld, Stop> {
    let (unused0, unused1) = ledgers
        .unused(pool)
        .await
        .map_err(|error| call_stop("the pool's unused balances", error))?;
    Ok(if zero_for_one {
        PoolHeld {
            ckusdc: unused0,
            token: unused1,
        }
    } else {
        PoolHeld {
            ckusdc: unused1,
            token: unused0,
        }
    })
}

/// Another driven payout whose leg at `pool` began and is not settled (no output, not
/// undone): until it is, what the pool holds of the canister's is not known to be whose, so
/// no other leg begins there. A payout stopped for a human is not driven: what it left at
/// the pool is in what the next leg reads before its swap, and never that leg's.
pub(crate) fn unsettled_at<S: Store>(
    state: &crate::state::State<S>,
    pool: Principal,
    except: PoolRef,
) -> Option<PoolRef> {
    let unsettled = |payout: &PayoutProgress| {
        payout.failed.is_none()
            && legs_at(payout).into_iter().any(|(at, leg)| {
                at == pool && leg.begun && leg.out.is_none() && leg.undone.is_none()
            })
    };
    let ledger = state.pool();
    let deposits = ledger.paying.iter().copied().filter(|r#ref| {
        state.store().pool_deposit(r#ref).is_some_and(|deposit| {
            deposit.status == DepositStatus::Paying && unsettled(&deposit.payout)
        })
    });
    let withdrawals = ledger.pending.iter().filter_map(|(id, op)| match op {
        PendingOp::Withdraw { payout, .. } if unsettled(payout) => Some(payout_ref(*id)),
        _ => None,
    });
    // an ICP swap's legs into ckUSDC
    let swaps = ledger.lines().open.into_iter().filter_map(|id| {
        let payout = state.store().pool_payout(id)?;
        payout
            .legs
            .as_ref()
            .is_some_and(unsettled)
            .then(|| types::pool::evm_payout_ref(id))
    });
    deposits
        .chain(withdrawals)
        .chain(swaps)
        .find(|r#ref| *r#ref != except)
}

/// Every leg of a payout's path and the pool it trades in: the first leg of a direct path;
/// a path through ICP's first leg and its way back (on the ICP/ckUSDC pool), and its second.
pub(crate) fn legs_at(progress: &PayoutProgress) -> Vec<(Principal, SwapLeg)> {
    match progress.attempt.map(|attempt| attempt.path) {
        Some(PayoutPath::Icpswap { pool }) => vec![(pool, progress.swap)],
        Some(PayoutPath::IcpswapVia { first, second, .. }) => vec![
            (first, progress.swap),
            (second, progress.leg(Some(LEG_SECOND))),
            (first, progress.leg(Some(LEG_BACK))),
        ],
        Some(PayoutPath::Ckusdc) | None => Vec::new(),
    }
}

/// A begun swap that did not answer with its output: what the pool holds of the canister's
/// above what it held just before the swap tells what happened, and only that is this
/// leg's. The token there: the swap happened and its output stayed at the pool; withdrawn,
/// and paid. ckUSDC there (all of the amount in, or any of it after the pool's own refusal):
/// the swap did not happen; withdrawn back, never more than the amount in, and all of it paid
/// as ckUSDC. Nothing there after the pool's own refusal (`refused`): the deposit never
/// happened, all of it paid as ckUSDC. Anything else, or a leg from before the pool's
/// holdings were read: the outcome is unknown, for a human. ICPSwap's unused balances and
/// its `withdraw` are not exercised against a live pool in this build.
async fn settle(
    ledgers: &impl Ledgers,
    settings: &PoolSettings,
    payout: &Payout,
    leg: LegAt,
    before: Option<PoolHeld>,
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
    // a leg from before the pool's holdings were read cannot tell its own from another's
    let before = before.ok_or_else(unknown)?;
    let zero_for_one = ledgers
        .direction(pool, ckusdc, payout.token_out)
        .await
        .map_err(|error| call_stop("the pool's metadata", error))?
        .ok_or_else(unknown)?;
    let now = held_at(ledgers, pool, zero_for_one).await?;
    let above = |now: TokenAmount, before: TokenAmount| {
        now.checked_sub(before).unwrap_or(TokenAmount::ZERO)
    };
    let (ck_left, token_left) = (
        above(now.ckusdc, before.ckusdc),
        above(now.token, before.token),
    );
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
                leg: None,
            },
            clock(),
        )?;
        return Ok((token_left, send));
    }
    let deposit = swap_in
        .checked_add(fee_ck)
        .ok_or_else(|| Stop::Failed("the deposit overflows".to_string()))?;
    if ck_left >= swap_in || (refused.is_some() && ck_left > TokenAmount::ZERO) {
        // this leg's own deposit, never more
        let withdrawn = ck_left.min(swap_in);
        ledgers
            .withdraw(pool, ckusdc, withdrawn, fee_ck)
            .await
            .map_err(|error| call_stop("the ckUSDC's withdrawal from the pool", error))?;
        // the withdrawal's transfer takes the ledger's fee out of what comes back
        let returned = withdrawn.checked_sub(fee_ck).unwrap_or(TokenAmount::ZERO);
        let cost = deposit.checked_sub(returned).unwrap_or(TokenAmount::ZERO);
        record(
            PoolStep::SwapUndone {
                r#ref: payout.id,
                cost,
                withdrawn,
                leg: None,
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
                    withdrawn: TokenAmount::ZERO,
                    leg: None,
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
