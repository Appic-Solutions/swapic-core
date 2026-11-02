//! The ICP staker's doors (spec 3.3: stake from ICP, withdraw on ICP) and the ICP pocket's
//! balance check (spec section 4).
//!
//! `stake_icp`: the caller's own ckUSDC, pulled on its approval of the canister. The TVL
//! room and the pull's dedup pair are recorded (`StakeStarted`) before the pull, and the
//! shares are minted (`StakedIcp`) at the pool's price before the pull, in the message that
//! reads its answer. A pull whose answer was lost is asked again by the drive, the same pull.
//!
//! `withdraw_icp`: the caller's shares, burned with the ckUSDC they are worth reserved
//! (`WithdrawStarted`) before any call, then the payout of Task 6 (same dedup, same all or
//! nothing), the withdrawer's amount paying every ledger fee. The ICP pocket must hold it:
//! a withdrawal larger than its free ckUSDC waits for the queue (P11b).
//!
//! The ICP pocket's check: every `books_check_interval`, the canister's ckUSDC balance read
//! beside what the pool's lines expect, less every payout started and not recorded paid (the
//! ledger may have debited it already), the lines read before and after the read and the
//! lower of the two expected. A shortfall beyond the tolerance latches the ICP latch, which
//! closes the pool until a controller reopens it. Every failure (no settings, a call that
//! failed, an answer that is no balance) is recorded as a failed check of its kind and
//! passes nothing: the pool's guard closes the pool when no check read a balance within
//! three intervals, as it does for Base.

#[cfg(test)]
mod tests;

use crate::ic::{self, Account};
use crate::pool::payout::{self, path_for, Busy, Live, Stop};
use crate::pool::{self, log, now};
use crate::state::{State, Store};
use crate::storage::events::read_state;
use crate::storage::pool as pool_store;
use crate::storage::timer_state;
use candid::{Nat, Principal};
use settlement_api::types::pool::{
    nat, PoolError, StakeIcpArgs, StakeIcpOk, WithdrawIcpArgs, WithdrawIcpOk,
};
use types::books::{assess, worth_recording};
use types::pool::rebalance::MoveDirection;
use types::pool::{
    op_id, payout_ref, DepositStatus, IcpAccount, IcpCheck, IcpReadFailure, PayoutAttempt,
    PayoutPath, PendingOp, PoolAccount, PoolStep, PrincipalBytes, Shares,
};
use types::{Address, Timestamp, TokenAmount};

/// Why the ICP pocket cannot pay a withdrawal now.
pub const POCKET_SHORT: &str = "the ICP pocket is short; withdrawals queue in P11b";

/// A stake as its caller asks it, read off the wire.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StakeRequest {
    pub caller: Principal,
    pub from_subaccount: Option<[u8; 32]>,
    pub amount: TokenAmount,
    pub owner: PoolAccount,
    /// Swapic's move fee out of `amount`.
    pub move_fee: TokenAmount,
}

impl StakeRequest {
    /// The wire arguments as `stake_icp` reads them, `caller` paying: what the door checks
    /// and what a consent message states.
    pub fn of(caller: Principal, args: StakeIcpArgs) -> Result<Self, PoolError> {
        Ok(Self {
            caller,
            from_subaccount: subaccount("from_subaccount", args.from_subaccount)?,
            amount: settlement_api::types::pool::amount("amount", &args.amount)?,
            owner: PoolAccount::try_from(args.owner)?,
            move_fee: settlement_api::types::pool::amount("move_fee", &args.move_fee)?,
        })
    }
}

/// A withdrawal as its caller asks it, read off the wire.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WithdrawRequest {
    pub caller: Principal,
    pub from: Option<[u8; 32]>,
    pub usdc: TokenAmount,
    pub max_shares: Shares,
    pub token_out: Principal,
    pub min_out: u128,
    pub to: IcpAccount,
    /// Swapic's move fee out of `usdc`.
    pub move_fee: TokenAmount,
}

impl WithdrawRequest {
    /// The wire arguments as `withdraw_icp` reads them, `caller` owning the shares: what the
    /// door checks and what a consent message states.
    pub fn of(caller: Principal, args: WithdrawIcpArgs) -> Result<Self, PoolError> {
        Ok(Self {
            caller,
            from: subaccount("from", args.from)?,
            usdc: settlement_api::types::pool::amount("usdc", &args.usdc)?,
            max_shares: Shares::new(settlement_api::types::pool::u128_of(
                "max_shares",
                &args.max_shares,
            )?),
            token_out: args.token_out,
            min_out: settlement_api::types::pool::u128_of("min_out", &args.min_out)?,
            to: args.to.try_into()?,
            move_fee: settlement_api::types::pool::amount("move_fee", &args.move_fee)?,
        })
    }

    /// The account whose shares are burned: the caller's, at `from`.
    pub fn owner(&self) -> IcpAccount {
        IcpAccount {
            owner: self.caller,
            subaccount: self.from,
        }
    }
}

fn screen_account(account: &PoolAccount, sanctioned: &impl Fn(&Address) -> bool) -> bool {
    match account {
        PoolAccount::Evm(address) => sanctioned(&Address::from(*address)),
        PoolAccount::Icp(account) => crate::pool::principal_sanctioned(account.owner, sanctioned),
    }
}

/// Every rule a stake meets before its pull: settings, an amount, no sanctioned party, and
/// room under the TVL cap with every stake on its way counted.
pub fn check_stake<S: Store>(
    state: &State<S>,
    request: &StakeRequest,
    sanctioned: impl Fn(&Address) -> bool,
) -> Result<(), PoolError> {
    let ledger = state.pool();
    let settings = ledger
        .settings
        .as_ref()
        .ok_or_else(|| PoolError::Closed("the pool has no settings".to_string()))?;
    if request.amount == TokenAmount::ZERO {
        return Err(PoolError::NotAllowed(
            "the amount must be above zero".to_string(),
        ));
    }
    // an ICP stake pays for one ICP to Base move, out of its amount
    settings
        .least_move_fee
        .admits(MoveDirection::IcpToBase, request.move_fee)
        .map_err(PoolError::NotAllowed)?;
    if request.move_fee >= request.amount {
        return Err(PoolError::NotAllowed(
            "the move fee must be below the amount".to_string(),
        ));
    }
    if crate::pool::principal_sanctioned(request.caller, &sanctioned)
        || screen_account(&request.owner, &sanctioned)
    {
        return Err(PoolError::Sanctioned(
            "a party to the stake is sanctioned".to_string(),
        ));
    }
    let would_be = ledger
        .tvl_measure()
        .checked_add(request.amount)
        .unwrap_or(TokenAmount::MAX);
    if would_be > settings.tvl_cap {
        return Err(PoolError::AboveTvlCap {
            cap: nat(settings.tvl_cap),
            would_be: nat(would_be),
        });
    }
    if ledger.mint_now(request.amount).is_none() {
        return Err(PoolError::NotAllowed(
            "the stake mints nothing at the pool's price".to_string(),
        ));
    }
    Ok(())
}

/// What a withdrawal burns, the path it pays by, and the exact transfer its payout starts
/// with: the path's first amount (`payout::path_amount`) on the ckUSDC ledger.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WithdrawalPlan {
    pub burned: Shares,
    pub path: PayoutPath,
    pub amount: TokenAmount,
    pub ckusdc: Principal,
}

/// How many ckUSDC fees each ICPSwap leg of a token path's withdrawal can cost at worst,
/// which its amount must more than cover (interface section 4: 4 direct, 8 through ICP): the
/// approval, the pool's pull, the withdrawal back from the pool of a swap that did not happen,
/// and the ckUSDC transfer it then falls back to.
pub const TOKEN_PATH_FEES: u8 = 4;

/// Every rule a withdrawal meets before anything is burned, in order: no sanctioned party,
/// an amount, the shares it burns (rounded up) within `max_shares` and the caller's own, the
/// pool open, the ICP pocket's free ckUSDC, a listed token, a ckUSDC payout at least
/// `min_out`, and a token path's amount above every fee the path can cost.
pub fn plan_withdrawal<S: Store>(
    state: &State<S>,
    request: &WithdrawRequest,
    closed: Option<String>,
    sanctioned: impl Fn(&Address) -> bool,
) -> Result<WithdrawalPlan, PoolError> {
    if crate::pool::principal_sanctioned(request.caller, &sanctioned)
        || crate::pool::principal_sanctioned(request.to.owner, &sanctioned)
    {
        return Err(PoolError::Sanctioned(
            "a party to the withdrawal is sanctioned".to_string(),
        ));
    }
    if request.usdc == TokenAmount::ZERO {
        return Err(PoolError::NotAllowed("usdc must be above zero".to_string()));
    }
    let ledger = state.pool();
    let settings = ledger
        .settings
        .clone()
        .ok_or_else(|| PoolError::Closed("the pool has no settings".to_string()))?;
    // an ICP withdrawal pays for one Base to ICP move, out of its amount
    settings
        .least_move_fee
        .admits(MoveDirection::BaseToIcp, request.move_fee)
        .map_err(PoolError::NotAllowed)?;
    if request.move_fee >= request.usdc {
        return Err(PoolError::NotAllowed(
            "the move fee must be below usdc".to_string(),
        ));
    }
    let owned = state
        .store()
        .pool_share(&PoolAccount::Icp(request.owner()).key())
        .map_or(Shares::ZERO, |line| line.shares);
    let burned = Shares::burn_for(request.usdc, ledger.total_assets(), ledger.total_shares)
        .ok_or_else(|| PoolError::NotAllowed("the pool's assets cannot pay that".to_string()))?;
    if burned > request.max_shares || burned > owned {
        return Err(PoolError::ShortShares {
            have: Nat::from(owned.min(request.max_shares).get()),
            need: Nat::from(burned.get()),
        });
    }
    if let Some(reason) = closed {
        return Err(PoolError::Closed(reason));
    }
    // a withdrawal waits behind the queued payouts
    if ledger.icp_available() < request.usdc {
        return Err(PoolError::Closed(POCKET_SHORT.to_string()));
    }
    // what the payout may spend: the withdrawal less Swapic's move fee
    let budget = request
        .usdc
        .checked_sub(request.move_fee)
        .unwrap_or(TokenAmount::ZERO);
    if !settings.is_listed(request.token_out) {
        return Err(PoolError::TokenNotListed(request.token_out));
    }
    let fee = settings
        .fee_of(settings.ckusdc_ledger)
        .ok_or_else(|| PoolError::Closed("no ckUSDC fee was read".to_string()))?;
    let path = path_for(&settings, request.token_out);
    match path {
        PayoutPath::Ckusdc => {
            let sent = budget.checked_sub(fee).unwrap_or(TokenAmount::ZERO);
            if sent == TokenAmount::ZERO || sent < TokenAmount::from(request.min_out) {
                return Err(PoolError::NotAllowed(
                    "the withdrawal pays below min_out after the ledger's fee".to_string(),
                ));
            }
        }
        PayoutPath::Icpswap { .. } | PayoutPath::IcpswapVia { .. } => {
            let legs: u8 = if matches!(path, PayoutPath::IcpswapVia { .. }) {
                2
            } else {
                1
            };
            let fees = fee
                .checked_mul(TOKEN_PATH_FEES * legs)
                .unwrap_or(TokenAmount::MAX);
            if budget <= fees {
                return Err(PoolError::NotAllowed(format!(
                    "the withdrawal does not cover the {fees} of fees its token's path can cost"
                )));
            }
        }
    }
    let amount = payout::path_amount(budget, path, fee).ok_or_else(|| {
        PoolError::NotAllowed("the withdrawal does not cover its fees".to_string())
    })?;
    Ok(WithdrawalPlan {
        burned,
        path,
        amount,
        ckusdc: settings.ckusdc_ledger,
    })
}

fn subaccount(field: &'static str, bytes: Option<Vec<u8>>) -> Result<Option<[u8; 32]>, PoolError> {
    bytes
        .map(|bytes| settlement_api::types::pool::bytes32(field, &bytes))
        .transpose()
        .map_err(PoolError::from)
}

/// `stake_icp`'s body.
pub async fn stake(
    args: StakeIcpArgs,
    sanctioned: impl Fn(&Address) -> bool,
) -> Result<StakeIcpOk, PoolError> {
    pool::require_running()?;
    pool::require_pool_open()?;
    // counted before anything is recorded: past the limit, nothing is written
    pool::callers::admit(ic_cdk::api::caller(), now())?;
    let request = StakeRequest::of(ic_cdk::api::caller(), args)?;
    let id = read_state(|state| -> Result<_, PoolError> {
        check_stake(state, &request, &sanctioned)?;
        Ok(op_id(
            false,
            &request.owner,
            state.meta().next_event_index.get(),
        ))
    })?;
    let _busy = Busy::take(id.into_bytes())
        .ok_or_else(|| PoolError::Unavailable("the stake is in progress".to_string()))?;
    pool_store::record_at(
        PoolStep::StakeStarted {
            id,
            owner: request.owner,
            from: IcpAccount {
                owner: request.caller,
                subaccount: request.from_subaccount,
            },
            amount: request.amount,
            created_at: now(),
            move_fee: request.move_fee,
        },
        now(),
    )
    .map_err(|error| PoolError::NotAllowed(error.to_string()))?;
    match payout::pull_stake(&Live, id, now).await {
        Ok(Some(shares)) => Ok(StakeIcpOk {
            shares: Nat::from(shares.get()),
        }),
        Ok(None) => Err(PoolError::Unavailable(
            "the stake is no longer in progress".to_string(),
        )),
        Err(Stop::Failed(why)) => Err(PoolError::Ledger(why)),
        Err(Stop::Append(why)) | Err(Stop::Retry(why)) => Err(PoolError::Unavailable(format!(
            "{why}; the canister asks again with the same pull"
        ))),
        Err(Stop::Later) | Err(Stop::Again) | Err(Stop::Queued) => Err(PoolError::Unavailable(
            "the canister asks again with the same pull".to_string(),
        )),
    }
}

/// `withdraw_icp`'s body.
pub async fn withdraw(
    args: WithdrawIcpArgs,
    sanctioned: impl Fn(&Address) -> bool,
) -> Result<WithdrawIcpOk, PoolError> {
    pool::require_running()?;
    let request = WithdrawRequest::of(ic_cdk::api::caller(), args)?;
    let closed = pool::pool_closed();
    let owner = PoolAccount::Icp(request.owner());
    let (id, plan) = read_state(|state| -> Result<_, PoolError> {
        let plan = plan_withdrawal(state, &request, closed, &sanctioned)?;
        let id = op_id(true, &owner, state.meta().next_event_index.get());
        Ok((id, plan))
    })?;
    let burned = plan.burned;
    let _busy = Busy::take(id.into_bytes())
        .ok_or_else(|| PoolError::Unavailable("the withdrawal is in progress".to_string()))?;
    let at = now();
    pool_store::record_at(
        PoolStep::WithdrawStarted {
            id,
            owner: request.owner(),
            burned,
            usdc: request.usdc,
            to: request.to,
            token_out: request.token_out,
            min_out: request.min_out,
            attempt: PayoutAttempt {
                created_at: at,
                memo: id.into_bytes(),
                path: plan.path,
                amount: Some(plan.amount),
                token: Some(PrincipalBytes(plan.ckusdc)),
            },
            move_fee: request.move_fee,
        },
        at,
    )
    .map_err(|error| PoolError::NotAllowed(error.to_string()))?;
    let continues = |why: &str| {
        PoolError::Unavailable(format!(
            "{burned} shares burned; the payout of {} continues on the canister's timer: {why}",
            request.usdc
        ))
    };
    match payout::drive_withdrawal(&Live, id, now, &payout::guard).await {
        Ok(Some((burned, paid))) => Ok(WithdrawIcpOk {
            burned: Nat::from(burned.get()),
            paid: nat(paid),
        }),
        Ok(None) => Err(continues("it is no longer pending")),
        Err(Stop::Failed(why)) => {
            payout::fail(payout_ref(id), why.clone(), now());
            Err(continues(&why))
        }
        Err(Stop::Append(why)) | Err(Stop::Retry(why)) => Err(continues(&why)),
        Err(Stop::Later) | Err(Stop::Again) | Err(Stop::Queued) => {
            Err(continues("the pool's quote was below the minimum"))
        }
    }
}

/// What may leave the ICP pocket for a payout started and not recorded paid, one stopped
/// for a human included (it is still between the two): a swap's whole hold (its payout and
/// every fee its path can cost), a withdrawal's whole amount.
pub fn pending_payouts<S: Store>(state: &State<S>) -> TokenAmount {
    let ledger = state.pool();
    let deposits = ledger
        .paying
        .iter()
        .filter_map(|r#ref| state.store().pool_deposit(r#ref))
        .filter(|deposit| {
            matches!(
                deposit.status,
                DepositStatus::Paying | DepositStatus::PayoutFailed(_)
            ) && deposit.payout.attempt.is_some()
        })
        .map(|deposit| deposit.held());
    let withdrawals = ledger.pending.values().filter_map(|op| match op {
        PendingOp::Withdraw { usdc, .. } => Some(*usdc),
        PendingOp::Stake { .. } => None,
    });
    // an ICP swap whose undo began: its ckUSDC refund may be debited already
    let refunds = ledger.lines().open.into_iter().filter_map(|id| {
        let payout = state.store().pool_payout(id)?;
        (matches!(payout.status, types::pool::PayoutStatus::Undoing(_))
            && payout.received.is_some())
        .then(|| {
            payout
                .icp_hold()
                .checked_add(payout.icp_fee())
                .unwrap_or(TokenAmount::MAX)
        })
    });
    deposits
        .chain(withdrawals)
        .chain(refunds)
        .try_fold(TokenAmount::ZERO, TokenAmount::checked_add)
        .unwrap_or(TokenAmount::MAX)
}

/// What the ICP pocket's balance should be now: its free and reserved lines, less every
/// payout in flight.
pub fn icp_lines<S: Store>(state: &State<S>) -> TokenAmount {
    state
        .pool()
        .icp_held()
        .checked_sub(pending_payouts(state))
        .unwrap_or(TokenAmount::ZERO)
}

/// What the check expects of a balance read between two readings of the lines, and whether
/// `observed` falls short of it beyond `tolerance`: the lower of the two, so money that came
/// in while the read was out (a stake's pull recorded during it) is never a shortfall, and a
/// payout recorded during it is in neither.
pub fn judge(
    before: TokenAmount,
    after: TokenAmount,
    observed: TokenAmount,
    tolerance: TokenAmount,
) -> (TokenAmount, bool) {
    assess(before.min(after), observed, tolerance)
}

/// [`judge`] of `observed` against the lines as they stand, read before and after alike.
#[cfg(test)]
pub fn icp_expected<S: Store>(
    state: &State<S>,
    observed: TokenAmount,
    tolerance: TokenAmount,
) -> (TokenAmount, bool) {
    let lines = icp_lines(state);
    judge(lines, lines, observed, tolerance)
}

/// The ICP pocket's balance check: nothing before the pool has settings.
pub async fn check_icp_pocket(tolerance: TokenAmount) {
    // before the pool has settings there is no pocket to check and nothing to say
    let Some(settings) = pool_store::ledger().settings else {
        return;
    };
    let before = read_state(icp_lines);
    let answer =
        ic::icrc1_balance_of(settings.ckusdc_ledger, Account::of(ic_cdk::api::id(), None)).await;
    // the lines read again after the await: the lower of the two is expected
    let after = read_state(icp_lines);
    note_icp_check(check_of(answer, before, after, tolerance), now());
}

/// Keeps one ICP check at `now`: a read that failed as the time it failed at, a balance read
/// as the pocket's last read, and in the log as well when it has news
/// (`types::books::worth_recording`). A shortfall's line latches the pool closed; a line the
/// log refuses keeps the read out too, so the guard sees the reads go stale.
pub fn note_icp_check(read: Result<IcpCheck, IcpReadFailure>, now: Timestamp) {
    if let Some(line) = check_log(&read) {
        log(&line);
    }
    let check = match read {
        Ok(check) => check,
        Err(_) => {
            timer_state::update(|state| {
                state.icp_read.failed_at = Some(now);
                ((), true)
            });
            return;
        }
    };
    let recorded = pool_store::ledger().icp_check.map(|last| last.surplus());
    if worth_recording(recorded, check.surplus(), check.shortfall) {
        if let Err(error) = pool_store::record_at(PoolStep::IcpChecked { check }, now) {
            log(&format!("the ICP pocket's check was not recorded: {error}"));
            return;
        }
    }
    timer_state::update(|state| {
        state.icp_read.read = Some((now, check));
        ((), true)
    });
}

/// The log line one ICP check is worth: a shortfall's alert, and a read that failed.
pub fn check_log(read: &Result<IcpCheck, IcpReadFailure>) -> Option<String> {
    match read {
        Ok(check) if check.shortfall => Some(format!(
            "ALERT: the ICP pocket holds {} ckUSDC and {} is expected; the pool closes",
            check.observed, check.expected
        )),
        Ok(_) => None,
        Err(failure) => Some(format!(
            "the ICP pocket's balance was not read: {failure:?}"
        )),
    }
}

/// What one ICP check read: a failure of its kind for a call that failed and an answer that
/// is no balance the lines hold, never a pass. Otherwise the balance judged against the
/// lower of the lines read before and after the read.
pub fn check_of(
    answer: Result<Nat, ic::CallError>,
    before: TokenAmount,
    after: TokenAmount,
    tolerance: TokenAmount,
) -> Result<IcpCheck, IcpReadFailure> {
    let balance = answer.map_err(|_| IcpReadFailure::CallFailed)?;
    let observed = TokenAmount::try_from(balance)
        .ok()
        .filter(|amount| amount.try_into_u128().is_some())
        .ok_or(IcpReadFailure::BadAnswer)?;
    let (expected, shortfall) = judge(before, after, observed, tolerance);
    Ok(IcpCheck {
        observed,
        expected,
        shortfall,
    })
}
