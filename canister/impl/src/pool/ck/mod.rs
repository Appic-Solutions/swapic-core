//! `swap_from_icp_ck`, the slow route out of ICP (d1, the ck minter fallback): when the pool
//! cannot carry a swap, the ck minter pays USDC on Ethereum to the recipient directly. The
//! canister signs nothing on Ethereum, spends no ETH there and reads no Ethereum: the money
//! left ICP at the burn. The route earns nothing: no premium, and a network fee at or above
//! the settings' floor pays the minter's gas in ckETH.
//!
//! The door pulls and sells as `swap_from_icp` does, to the `min_usdc` it was given. Then, each
//! line before the call it guards:
//! - `PayoutCkWithdrawing`: `icp.reserved` and `owed` fall by the hold before any of it leaves,
//!   so the hourly check only ever sees a surplus. Both ledger fees of the way out come out of
//!   the hold (types' `CkLeg`), so the main account falls by exactly that and the network fee
//!   stays whole on `fees_icp`.
//! - The hold goes to the payout's own burn subaccount, with a dedup pair, the minter is
//!   approved there and on ckETH for at most `ck_swap_fee_allowance`, a higher gas price failing
//!   with nothing burned, and `withdraw_erc20` burns from the subaccount. [`AtMinter`] holds all
//!   of it: the minter takes one call at a time from a caller and traps on a second.
//! - `PayoutCkWithdrawn`, by the minter's answer or, after a lost one, by `BySenderAccount`.
//! - `PayoutCkLanded` at the minter's `TxFinalized`, which can take days: it is read every
//!   pass for an hour, then every ten minutes. The watcher tells the user from the receipt.
//!
//! The rule that keeps stakers whole: **nothing is undone on a word, only on proof**. A trap
//! reaches the canister as an unknown outcome, every minter guard is one, and a refusal to a
//! later call says nothing of an earlier one. So the minter is never asked again before its
//! list for the subaccount is read, and a way back begins only when the minter reimbursed the
//! withdrawal, or it lists nothing and the subaccount still holds the whole sum
//! ([`way_back`]). Anything else stops for a controller (`resolve_ck_withdrawal`).
//!
//! The way back (`PayoutCkReimbursed`, then the undo) brings the ckUSDC to the main account and
//! refunds what came back. The fees of the trip are the user's. The network fee goes back with
//! it when nothing burned, and stays when the minter burned, as its gas is spent. The step is
//! recorded before the transfer, so what comes back and its dedup pair are fixed for every
//! retry, however late the reimbursement: the undo's own line keeps the hold out of the hourly
//! check until then.

#[cfg(test)]
mod tests;

use crate::ic::{self, Account, CallError, Dedup, WithdrawErc20Error, WithdrawFailure};
use crate::pool::icp_swap::{self, check_payout_terms, MAX_DEADLINE, MIN_DEADLINE};
use crate::pool::payout::{open, record, Busy, Ledgers, Live, Stop};
use crate::pool::plan::PlanReads;
use crate::pool::rebalance::drive::{MinterSays, LATE_READ_EVERY, STUCK_AFTER};
use crate::pool::{self, log, now};
use crate::state::{State, Store};
use crate::storage::events::read_state;
use crate::storage::{flow_settings, pool as pool_store, timer_state};
use candid::{Nat, Principal};
use settlement_api::types::pool::{
    amount, bytes32, evm_address, CkResolution, CkSwapTerms, PoolError, SwapFromIcpOk,
};
use std::cell::Cell;
use std::time::Duration;
use types::hash::TxHash;
use types::pool::payout::{CkLeg, EvmPayout, IcpSwapTerms, PayoutKind, PayoutStatus, PayoutTerms};
use types::pool::{
    ck_back_memo, ck_burn, ck_least_usdc, ck_memo, ck_subaccount, evm_payout_ref, Flag, IcpAccount,
    PayoutResolution, PoolStep, Undo,
};
use types::{Address, ChainId, EvmAddress, Timestamp, TokenAmount};

/// How often a withdrawal that was not taken is asked again: each ask approves ckETH, a ledger
/// fee the float pays.
pub const ASK_EVERY: Duration = Duration::from_secs(120);

/// How long after its withdrawal began the minter is read on every pass, burned or not.
pub const FAST_READS: Duration = Duration::from_secs(3_600);

/// How long past its withdrawal's start the minter's silence is a log line and no more: its
/// `TxFinalized` lagged 7.8 days at worst, and the row stays open and read past this.
pub const ALERT_AFTER: Duration = Duration::from_secs(8 * 86_400);

thread_local! {
    // Heap by necessity: whether a withdrawal is at the minter right now. The minter's guard
    // traps a caller's second call, and that trap reads as an unknown outcome.
    static AT_MINTER: Cell<bool> = const { Cell::new(false) };
}

/// Held over a whole withdrawal, moves included (its approvals and its call), released on
/// drop, a trap included: the canister is one caller to the minter, and one ckETH allowance.
pub struct AtMinter(());

impl AtMinter {
    pub fn take() -> Option<Self> {
        // lazily: a guard built for a refusal would release the holder's on its drop
        (!AT_MINTER.with(|held| held.replace(true))).then(|| Self(()))
    }
}

impl Drop for AtMinter {
    fn drop(&mut self) {
        AT_MINTER.with(|held| held.set(false));
    }
}

/// The calls a ck withdrawal makes, so the driver runs the same against the ledgers and the
/// minter, and against a stub.
#[allow(async_fn_in_trait)]
pub trait CkCalls {
    /// The canister's own principal: it owns the burn subaccounts.
    fn own_principal(&self) -> Principal;
    async fn ck_balance(
        &self,
        ledger: Principal,
        account: Account,
    ) -> Result<TokenAmount, CallError>;
    async fn ck_transfer(
        &self,
        ledger: Principal,
        from_subaccount: Option<[u8; 32]>,
        to: Account,
        amount: TokenAmount,
        dedup: Dedup,
    ) -> Result<u64, CallError>;
    async fn ck_approve(
        &self,
        ledger: Principal,
        from_subaccount: Option<[u8; 32]>,
        spender: Principal,
        amount: TokenAmount,
        dedup: Dedup,
    ) -> Result<(), CallError>;
    async fn ck_withdraw(
        &self,
        minter: Principal,
        ledger: Principal,
        amount: TokenAmount,
        recipient: String,
        from_subaccount: [u8; 32],
    ) -> Result<ic::RetrieveErc20Request, WithdrawFailure>;
    /// The minter's withdrawals from `account`: `BySenderAccount`.
    async fn ck_listed(
        &self,
        minter: Principal,
        account: Account,
    ) -> Result<Vec<ic::WithdrawalDetail>, CallError>;
    /// The minter's view of one withdrawal: `ByWithdrawalId`.
    async fn ck_status(
        &self,
        minter: Principal,
        id: u64,
    ) -> Result<Vec<ic::WithdrawalDetail>, CallError>;
}

impl CkCalls for Live {
    fn own_principal(&self) -> Principal {
        ic_cdk::api::id()
    }

    async fn ck_balance(
        &self,
        ledger: Principal,
        account: Account,
    ) -> Result<TokenAmount, CallError> {
        let balance = ic::icrc1_balance_of(ledger, account).await?;
        TokenAmount::try_from(balance)
            .map_err(|_| CallError::Unknown("the balance does not fit".to_string()))
    }

    async fn ck_transfer(
        &self,
        ledger: Principal,
        from_subaccount: Option<[u8; 32]>,
        to: Account,
        amount: TokenAmount,
        dedup: Dedup,
    ) -> Result<u64, CallError> {
        ic::icrc1_transfer(ledger, from_subaccount, to, Nat::from(amount), dedup).await
    }

    async fn ck_approve(
        &self,
        ledger: Principal,
        from_subaccount: Option<[u8; 32]>,
        spender: Principal,
        amount: TokenAmount,
        dedup: Dedup,
    ) -> Result<(), CallError> {
        ic::icrc2_approve_once(ledger, from_subaccount, spender, Nat::from(amount), dedup).await
    }

    async fn ck_withdraw(
        &self,
        minter: Principal,
        ledger: Principal,
        amount: TokenAmount,
        recipient: String,
        from_subaccount: [u8; 32],
    ) -> Result<ic::RetrieveErc20Request, WithdrawFailure> {
        ic::withdraw_erc20(
            minter,
            ledger,
            Nat::from(amount),
            recipient,
            Some(from_subaccount),
        )
        .await
    }

    async fn ck_listed(
        &self,
        minter: Principal,
        account: Account,
    ) -> Result<Vec<ic::WithdrawalDetail>, CallError> {
        ic::withdrawals_of(minter, account).await
    }

    async fn ck_status(
        &self,
        minter: Principal,
        id: u64,
    ) -> Result<Vec<ic::WithdrawalDetail>, CallError> {
        ic::withdrawal_status(minter, id).await
    }
}

/// A swap through the minter as its caller asks it, read off the wire.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CkRequest {
    pub caller: Principal,
    pub token_in: Principal,
    pub amount_in: TokenAmount,
    /// The caller's account the token is pulled from.
    pub from: IcpAccount,
    pub refund_to: IcpAccount,
    pub terms: PayoutTerms,
    pub min_usdc: TokenAmount,
    pub network_fee: TokenAmount,
}

/// The wire terms as the domain takes them: the caller's account at `from_subaccount` is
/// pulled from, `refund_to` is the caller's account unless named.
pub fn request_of(caller: Principal, args: CkSwapTerms) -> Result<CkRequest, PoolError> {
    let from_subaccount = args
        .from_subaccount
        .map(|bytes| bytes32("from_subaccount", &bytes))
        .transpose()?;
    let from = IcpAccount {
        owner: caller,
        subaccount: from_subaccount,
    };
    let refund_to = match args.refund_to {
        Some(account) => {
            let account: IcpAccount = account.try_into()?;
            crate::pool::not_anonymous("the refund account", account.owner)?;
            account
        }
        None => from,
    };
    Ok(CkRequest {
        caller,
        token_in: args.token_in,
        amount_in: amount("amount_in", &args.amount_in)?,
        from,
        refund_to,
        terms: PayoutTerms {
            dst_chain: ChainId::new(args.dst_chain),
            recipient: evm_address("recipient", &args.recipient)?,
            token_out: evm_address("token_out", &args.token_out)?,
            min_out: amount("min_out", &args.min_out)?,
            deadline: Timestamp::from_nanos(args.deadline_ns),
        },
        min_usdc: amount("min_usdc", &args.min_usdc)?,
        network_fee: amount("network_fee", &args.network_fee)?,
    })
}

/// The door's own terms of a request that met every rule.
fn refused(why: impl Into<String>) -> PoolError {
    PoolError::NotAllowed(why.into())
}

/// Every rule a swap through the minter meets before anything is recorded: the pool's and the
/// payouts' settings, the route open, a listed token, an amount, no sanctioned party, a
/// deadline 5 minutes to 1 hour away, USDC on Ethereum to a recipient that is none of
/// Swapic's, the network fee at the route's floor, and `min_usdc` covering `min_out`, that fee
/// and both ledger fees. Neither whether the pool takes swaps nor its Base pocket matters: this
/// route exists for when they cannot carry the swap.
pub fn check<S: Store>(
    state: &State<S>,
    config: &types::Config,
    request: &CkRequest,
    now: Timestamp,
    canister: Option<EvmAddress>,
    sanctioned: impl Fn(&Address) -> bool,
) -> Result<IcpSwapTerms, PoolError> {
    let ledger = state.pool();
    let settings = ledger
        .settings
        .as_ref()
        .ok_or_else(|| PoolError::Closed("the pool has no settings".to_string()))?;
    let lines = ledger.lines();
    let payouts = lines
        .settings
        .ok_or_else(|| PoolError::Closed("the payouts have no settings".to_string()))?;
    let ck = payouts
        .ck
        .filter(|ck| ck.open)
        .ok_or_else(|| PoolError::Closed("the ck route is closed".to_string()))?;
    let path = settings
        .path_of(request.token_in)
        .ok_or(PoolError::TokenNotListed(request.token_in))?;
    if request.amount_in == TokenAmount::ZERO {
        return Err(refused("amount_in must be above zero"));
    }
    if pool::principal_sanctioned(request.caller, &sanctioned)
        || pool::principal_sanctioned(request.refund_to.owner, &sanctioned)
    {
        return Err(PoolError::Sanctioned(
            "a party to the swap is sanctioned".to_string(),
        ));
    }
    let after = |wait: Duration| {
        now.checked_add(wait)
            .unwrap_or(Timestamp::from_nanos(u64::MAX))
    };
    if request.terms.deadline < after(MIN_DEADLINE) {
        return Err(PoolError::Expired);
    }
    if request.terms.deadline > after(MAX_DEADLINE) {
        return Err(refused("the deadline is more than an hour away"));
    }
    if request.terms.dst_chain != ChainId::ETHEREUM {
        return Err(refused("the ck route pays on Ethereum only"));
    }
    let usdc = config
        .evm_chains
        .get(ChainId::ETHEREUM)
        .map(|chain| chain.usdc)
        .ok_or_else(|| PoolError::Closed("Ethereum is not configured".to_string()))?;
    if request.terms.token_out != usdc {
        return Err(refused("the ck route pays USDC only"));
    }
    check_payout_terms(config, &request.terms, canister, &sanctioned)?;
    if request.network_fee < ck.min_network_fee {
        return Err(refused(format!(
            "the network fee must be at least {}",
            ck.min_network_fee
        )));
    }
    let fee = settings
        .fee_of(settings.ckusdc_ledger)
        .ok_or_else(|| PoolError::Closed("no ckUSDC fee was read".to_string()))?;
    if request.terms.min_out <= fee {
        return Err(refused("min_out must be above the ledger's fee"));
    }
    let least = ck_least_usdc(request.terms.min_out, request.network_fee, fee)
        .ok_or_else(|| refused("min_out and the fees are too large"))?;
    if request.min_usdc < least {
        return Err(refused(format!(
            "min_usdc must be at least {least}: min_out, the network fee and both ledger fees"
        )));
    }
    if path == types::pool::RoutePath::Ckusdc && request.min_usdc > request.amount_in {
        return Err(refused("a ckUSDC swap's min_usdc is at most what it sends"));
    }
    Ok(IcpSwapTerms {
        token_in: request.token_in,
        amount_in: request.amount_in,
        from: request.from,
        min_usdc: request.min_usdc,
        premium: TokenAmount::ZERO,
        network_fee: request.network_fee,
        refund_to: request.refund_to,
        slippage_bps: None,
        planned: None,
    })
}

/// The minter, the ckETH ledger and the most its gas may burn, as a step needs them.
struct Minter {
    minter: Principal,
    cketh: Principal,
    allowance: TokenAmount,
}

fn minter_of() -> Result<Minter, String> {
    let flow = flow_settings::get();
    Ok(Minter {
        minter: flow.ck_minter.ok_or("no ck minter is configured")?,
        cketh: flow.cketh_ledger.ok_or("no ckETH ledger is configured")?,
        allowance: pool_store::ledger()
            .lines()
            .settings
            .and_then(|settings| settings.ck)
            .ok_or("the ck route has no settings")?
            .ck_swap_fee_allowance,
    })
}

/// Whether the canister's ckETH can pay one withdrawal's gas. Short, a withdrawal waits and
/// the float is alerted on.
async fn float_holds(calls: &impl CkCalls, minter: &Minter) -> Result<bool, Stop> {
    let main = Account::of(calls.own_principal(), None);
    let balance = calls
        .ck_balance(minter.cketh, main)
        .await
        .map_err(|error| Stop::Retry(format!("the ckETH balance: {error}")))?;
    if balance < minter.allowance {
        log(&format!(
            "ALERT: the canister holds {balance} ckETH, below the {} a withdrawal's gas may take",
            minter.allowance
        ));
    }
    Ok(balance >= minter.allowance)
}

/// `swap_from_icp_ck`'s body: the payout's id and where it stands. A pull the ledger refused
/// answers its refusal; anything else answers the id, and `payout(id)` tells where it stands.
pub async fn swap_from_icp_ck(
    args: CkSwapTerms,
    sanctioned: impl Fn(&Address) -> bool,
) -> Result<SwapFromIcpOk, PoolError> {
    pool::require_running()?;
    pool::require_pool_open()?;
    let caller = ic_cdk::api::caller();
    // a caller the limit could not tell apart from any other
    if caller == Principal::anonymous() {
        return Err(refused("the anonymous caller cannot swap"));
    }
    let request = request_of(caller, args)?;
    let config = crate::storage::config::get();
    let canister = crate::storage::ecdsa_address::get();
    read_state(|state| check(state, &config, &request, now(), canister, &sanctioned))?;
    // the float before the user's money: a withdrawal cannot start on an empty one
    let minter = minter_of().map_err(PoolError::Closed)?;
    if !float_holds(&Live, &minter)
        .await
        .map_err(|_| PoolError::Unavailable("the ckETH balance was not read".to_string()))?
    {
        return Err(PoolError::Unavailable(
            "the ck route is short of ckETH now".to_string(),
        ));
    }
    // read again after the await, then the money first: nothing is written before it is in
    // (audit C15)
    pool::require_running()?;
    pool::require_pool_open()?;
    let at = now();
    read_state(|state| check(state, &config, &request, at, canister, &sanctioned))?;
    crate::pool::pull_first::worth_pulling(request.token_in, request.amount_in)?;
    let pair = crate::pool::pull_first::dedup(
        "swap_from_icp_ck",
        &request.from,
        request.token_in,
        request.amount_in,
        at,
    );
    let block = crate::pool::pull_first::pull(
        "swap_from_icp_ck",
        request.token_in,
        request.from,
        request.amount_in,
        pair,
        at,
    )
    .await?;
    // the money is in: its lines in this one message, the checks run again on the pool as it
    // is now; money no line takes goes back at once
    let at = now();
    let opened = (|| -> Result<u64, PoolError> {
        // the pool may have closed while the pull was out: then the money goes back
        pool::require_running()?;
        pool::require_pool_open()?;
        let (id, terms) = read_state(|state| -> Result<(u64, IcpSwapTerms), PoolError> {
            let terms = check(state, &config, &request, at, canister, &sanctioned)?;
            Ok((state.pool().lines().next_id, terms))
        })?;
        let settings = pool_store::ledger()
            .settings
            .ok_or_else(|| PoolError::Closed("the pool has no settings".to_string()))?;
        pool_store::record_at(
            PoolStep::PayoutOpened {
                id,
                kind: Box::new(PayoutKind::IcpSwap(terms)),
                terms: request.terms,
                undo: Undo::Refund {
                    to: terms.refund_to,
                },
                plan: None,
                ck: Flag(true),
            },
            at,
        )
        .map_err(|error| PoolError::NotAllowed(error.to_string()))?;
        icp_swap::pulled_line(
            id,
            &settings,
            at,
            block,
            request.token_in,
            request.amount_in,
        )?;
        Ok(id)
    })();
    let id = match opened {
        Ok(id) => id,
        Err(why) => {
            let fee = pool_store::ledger()
                .settings
                .and_then(|settings| settings.fee_of(request.token_in));
            crate::pool::pull_first::give_back(
                "swap_from_icp_ck",
                request.token_in,
                request.from,
                request.amount_in,
                fee,
                pair,
                &format!("{why:?}"),
            )
            .await;
            return Err(why);
        }
    };
    if let Some(_busy) = Busy::take(evm_payout_ref(id).into_bytes()) {
        // whatever stops it, the payout timer carries it on
        let _ = drive(&Live, id, now, &crate::pool::payout::guard).await;
    }
    icp_swap::answer(id)
}

/// A step's way on.
enum Flow {
    /// The payout moved: the drive looks at it again.
    On,
    /// Nothing more for now.
    Wait,
}

/// A ck swap's next steps, as far as they go now: its pull and legs, the withdrawal, the
/// minter's word, and a way back with its refund.
pub async fn drive(
    calls: &(impl Ledgers + PlanReads + CkCalls),
    id: u64,
    clock: impl Fn() -> Timestamp + Copy,
    guard: &impl Fn() -> Result<(), String>,
) -> Result<(), Stop> {
    for _ in 0..8 {
        let Some(payout) = pool_store::payout(id) else {
            return Ok(());
        };
        let Some(ck) = payout.ck else {
            return Ok(());
        };
        match &payout.status {
            PayoutStatus::Opened | PayoutStatus::Swapping => {
                icp_swap::drive(calls, id, clock, guard).await?;
                if pool_store::payout(id).is_none_or(|now| now.status == payout.status) {
                    return Ok(());
                }
            }
            PayoutStatus::Known => match begin(calls, &payout, clock, guard).await? {
                Flow::On => {}
                Flow::Wait => return Ok(()),
            },
            PayoutStatus::CkWithdrawing => {
                match withdraw(calls, &payout, ck, clock, guard).await? {
                    Flow::On => {}
                    Flow::Wait => return Ok(()),
                }
            }
            PayoutStatus::CkWithdrawn => match read_minter(calls, &payout, ck, clock).await? {
                Flow::On => {}
                Flow::Wait => return Ok(()),
            },
            PayoutStatus::Undoing(_) => {
                if ck.back.is_some() {
                    bring_back(calls, &payout, ck, clock).await?;
                }
                icp_swap::drive(calls, id, clock, guard).await?;
                return Ok(());
            }
            _ => return Ok(()),
        }
    }
    Ok(())
}

/// Its withdrawal begins, the deadline still ahead (past it, the pass undoes it): the sum it
/// burns fixed, its lines down. Waits at no cost while the pool is closed or the float short.
async fn begin(
    calls: &impl CkCalls,
    payout: &EvmPayout,
    clock: impl Fn() -> Timestamp + Copy,
    guard: &impl Fn() -> Result<(), String>,
) -> Result<Flow, Stop> {
    open(guard)?;
    let at = clock();
    if at >= payout.terms.deadline {
        return Ok(Flow::Wait);
    }
    let minter = minter_of().map_err(Stop::Retry)?;
    let settings = pool_store::ledger()
        .settings
        .ok_or_else(|| Stop::Retry("the pool has no settings".to_string()))?;
    let fee = settings
        .fee_of(settings.ckusdc_ledger)
        .ok_or_else(|| Stop::Failed("no ckUSDC fee was read".to_string()))?;
    // the ledger's fee can rise after the door: below `min_out` it never begins
    let Some(amount) =
        ck_burn(payout.icp_hold(), fee).filter(|amount| *amount >= payout.terms.min_out)
    else {
        icp_swap::undo(
            payout.id,
            "the ledger's fees take it below min_out".to_string(),
            at,
        )?;
        return Ok(Flow::On);
    };
    if !float_holds(calls, &minter).await? {
        return Err(Stop::Retry("short of ckETH".to_string()));
    }
    open(guard)?;
    record(
        PoolStep::PayoutCkWithdrawing {
            id: payout.id,
            created_at: at,
            amount,
            fee,
        },
        at,
    )?;
    Ok(Flow::On)
}

/// The subaccount's account, and the withdrawal's fixed numbers.
struct Fixed {
    sub: [u8; 32],
    account: Account,
    main: Account,
    amount: TokenAmount,
    funding: TokenAmount,
    dedup: Dedup,
    ckusdc: Principal,
}

fn fixed(calls: &impl CkCalls, payout: &EvmPayout, ck: CkLeg) -> Result<Fixed, Stop> {
    let ckusdc = pool_store::ledger()
        .settings
        .ok_or_else(|| Stop::Retry("the pool has no settings".to_string()))?
        .ckusdc_ledger;
    let broken = || Stop::Failed("the withdrawal's sums are not fixed".to_string());
    let sub = ck_subaccount(payout.id);
    let me = calls.own_principal();
    Ok(Fixed {
        sub,
        account: Account::of(me, Some(sub)),
        main: Account::of(me, None),
        amount: ck.amount.ok_or_else(broken)?,
        funding: ck.funding().ok_or_else(broken)?,
        dedup: Dedup {
            memo: ck_memo(payout.id),
            created_at_nanos: ck.created_at.ok_or_else(broken)?.as_nanos(),
        },
        ckusdc,
    })
}

/// The minter's one withdrawal from the payout's subaccount, if it lists one.
async fn listed(
    calls: &impl CkCalls,
    minter: Principal,
    account: &Account,
) -> Result<Option<u64>, CallError> {
    let details = calls.ck_listed(minter, account.clone()).await?;
    Ok(details.iter().map(|detail| detail.withdrawal_id).min())
}

/// A withdrawal that may or may not have burned: what the minter lists decides first, always,
/// and only then is it asked. Past the deadline, or on a refusal that cannot pass, the way
/// back. A minter that cannot be read waits, and stops for a controller in the end.
async fn withdraw(
    calls: &impl CkCalls,
    payout: &EvmPayout,
    ck: CkLeg,
    clock: impl Fn() -> Timestamp + Copy,
    guard: &impl Fn() -> Result<(), String>,
) -> Result<Flow, Stop> {
    let Some(_at_minter) = AtMinter::take() else {
        return Err(Stop::Retry(
            "another withdrawal is at the ck minter".to_string(),
        ));
    };
    let id = payout.id;
    let minter = minter_of().map_err(Stop::Retry)?;
    let fixed = fixed(calls, payout, ck)?;
    let found = match listed(calls, minter.minter, &fixed.account).await {
        Ok(found) => found,
        Err(error) => return read_failed(payout, ck, &error.to_string(), clock()),
    };
    if let Some(withdrawal_id) = found {
        record(PoolStep::PayoutCkWithdrawn { id, withdrawal_id }, clock())?;
        return Ok(Flow::On);
    }
    let now = clock();
    if now >= payout.terms.deadline {
        return way_back(calls, payout, &fixed, minter.minter, clock).await;
    }
    if !due(id, now, ASK_EVERY) {
        return Ok(Flow::Wait);
    }
    if !float_holds(calls, &minter).await? {
        return Err(Stop::Retry("short of ckETH".to_string()));
    }
    open(guard)?;
    match calls
        .ck_transfer(
            fixed.ckusdc,
            None,
            fixed.account.clone(),
            fixed.funding,
            fixed.dedup,
        )
        .await
    {
        Ok(_) => {}
        Err(CallError::Unknown(why)) => return Err(Stop::Retry(format!("the funding: {why}"))),
        // never landed, or it can no longer be asked: the way back decides on what it reads
        Err(error) => {
            log(&format!("ALERT: the ck payout {id}'s funding: {error}"));
            return way_back(calls, payout, &fixed, minter.minter, clock).await;
        }
    }
    let approval = fixed
        .amount
        .checked_add(ck.fee.unwrap_or(TokenAmount::ZERO))
        .ok_or_else(|| Stop::Failed("the approval overflows".to_string()))?;
    match calls
        .ck_approve(
            fixed.ckusdc,
            Some(fixed.sub),
            minter.minter,
            approval,
            fixed.dedup,
        )
        .await
    {
        Ok(()) => {}
        Err(CallError::Unknown(why)) => return Err(Stop::Retry(format!("the approval: {why}"))),
        Err(error) => {
            log(&format!("ALERT: the ck payout {id}'s approval: {error}"));
            return way_back(calls, payout, &fixed, minter.minter, clock).await;
        }
    }
    // its own instant for each ask: a move's larger allowance must not stand behind a duplicate
    let gas = Dedup {
        memo: fixed.dedup.memo,
        created_at_nanos: clock().as_nanos(),
    };
    calls
        .ck_approve(minter.cketh, None, minter.minter, minter.allowance, gas)
        .await
        .map_err(|error| Stop::Retry(format!("the ckETH approval: {error}")))?;
    open(guard)?;
    match calls
        .ck_withdraw(
            minter.minter,
            fixed.ckusdc,
            fixed.amount,
            payout.terms.recipient.to_string(),
            fixed.sub,
        )
        .await
    {
        Ok(request) => {
            let withdrawal_id = u64::try_from(request.cketh_block_index.0).unwrap_or(u64::MAX);
            record(PoolStep::PayoutCkWithdrawn { id, withdrawal_id }, clock())?;
            Ok(Flow::On)
        }
        // an alert for the float, and asked again until the deadline
        Err(WithdrawFailure::Refused(refusal)) if refusal.may_pass() => {
            if matches!(refusal, WithdrawErc20Error::CkEthLedgerError { .. }) {
                log(&format!("ALERT: the ck payout {id} waits: {refusal}"));
            }
            Ok(Flow::Wait)
        }
        Err(WithdrawFailure::Refused(refusal)) => {
            log(&format!("the ck payout {id} goes back: {refusal}"));
            way_back(calls, payout, &fixed, minter.minter, clock).await
        }
        // never read as nothing burned, nor asked again before the minter's list says
        Err(WithdrawFailure::Unknown(why)) => {
            Err(Stop::Retry(format!("the minter's answer was lost: {why}")))
        }
    }
}

/// The minter could not be read: asked again next pass, and a controller's in the end.
fn read_failed(payout: &EvmPayout, ck: CkLeg, why: &str, now: Timestamp) -> Result<Flow, Stop> {
    let begun = ck.created_at.unwrap_or(payout.opened_at);
    if now.saturating_duration_since(begun) >= STUCK_AFTER {
        let why = format!("ALERT: the ck minter's list was not read: {why}");
        log(&format!("the ck payout {}: {why}", payout.id));
        icp_swap::stop(payout.id, &why, now)?;
    }
    Err(Stop::Retry(format!("the minter's list: {why}")))
}

/// The one gate back: the undo starts only on reads, whatever any call answered. The minter
/// lists nothing, and the whole sum is in the subaccount, or its funding never landed. A
/// withdrawal it lists is adopted. Anything else may be burned, and stops for a controller.
async fn way_back(
    calls: &impl CkCalls,
    payout: &EvmPayout,
    fixed: &Fixed,
    minter: Principal,
    clock: impl Fn() -> Timestamp + Copy,
) -> Result<Flow, Stop> {
    let id = payout.id;
    let found = listed(calls, minter, &fixed.account)
        .await
        .map_err(|error| Stop::Retry(format!("the minter's list: {error}")))?;
    if let Some(withdrawal_id) = found {
        record(PoolStep::PayoutCkWithdrawn { id, withdrawal_id }, clock())?;
        return Ok(Flow::On);
    }
    let mut left = subaccount_balance(calls, fixed).await?;
    if left < fixed.amount {
        // asked as the same transfer: a funding that landed answers as one and moves nothing
        match calls
            .ck_transfer(
                fixed.ckusdc,
                None,
                fixed.account.clone(),
                fixed.funding,
                fixed.dedup,
            )
            .await
        {
            Ok(_) => {
                left = subaccount_balance(calls, fixed).await?;
                if left < fixed.amount {
                    let why = "ALERT: the subaccount is short and the minter lists no withdrawal";
                    log(&format!("the ck payout {id}: {why}"));
                    icp_swap::stop(id, why, clock())?;
                    return Ok(Flow::Wait);
                }
            }
            // nothing ever moved: nothing left the main account
            Err(CallError::Refused(_)) => left = TokenAmount::ZERO,
            Err(error @ CallError::Unknown(_)) => {
                return Err(Stop::Retry(format!("the funding: {error}")))
            }
            Err(error @ CallError::Expired(_)) => {
                let why = format!("ALERT: the funding's outcome can no longer be known: {error}");
                log(&format!("the ck payout {id}: {why}"));
                icp_swap::stop(id, &why, clock())?;
                return Ok(Flow::Wait);
            }
        }
    }
    record(
        PoolStep::PayoutCkReimbursed {
            id,
            left,
            block: None,
            burned: false,
        },
        clock(),
    )?;
    Ok(Flow::On)
}

async fn subaccount_balance(calls: &impl CkCalls, fixed: &Fixed) -> Result<TokenAmount, Stop> {
    calls
        .ck_balance(fixed.ckusdc, fixed.account.clone())
        .await
        .map_err(|error| Stop::Retry(format!("the subaccount's balance: {error}")))
}

/// A burned withdrawal: only the minter's word ends it. `TxFinalized` success closes the row,
/// a reimbursement starts the way back, and its gas stays spent. Days can pass, so after the
/// first hour it is read every ten minutes.
async fn read_minter(
    calls: &impl CkCalls,
    payout: &EvmPayout,
    ck: CkLeg,
    clock: impl Fn() -> Timestamp + Copy,
) -> Result<Flow, Stop> {
    let id = payout.id;
    let now = clock();
    let begun = ck.created_at.unwrap_or(payout.opened_at);
    let waited = now.saturating_duration_since(begun);
    if waited >= FAST_READS && !due(id, now, LATE_READ_EVERY) {
        return Ok(Flow::Wait);
    }
    let minter = flow_settings::get()
        .ck_minter
        .ok_or_else(|| Stop::Retry("no ck minter is configured".to_string()))?;
    let withdrawal_id = ck
        .withdrawal_id
        .ok_or_else(|| Stop::Failed("the payout has no withdrawal".to_string()))?;
    let details = calls
        .ck_status(minter, withdrawal_id)
        .await
        .map_err(|error| Stop::Retry(format!("the minter's withdrawal: {error}")))?;
    let Some(detail) = details
        .into_iter()
        .find(|detail| detail.withdrawal_id == withdrawal_id)
    else {
        log(&format!(
            "the ck payout {id}: the minter knows no withdrawal {withdrawal_id}"
        ));
        return Ok(Flow::Wait);
    };
    match MinterSays::of(detail.status).map_err(Stop::Retry)? {
        MinterSays::Succeeded(tx_hash) => {
            record(
                PoolStep::PayoutCkLanded {
                    id,
                    tx_hash,
                    by_controller: false,
                },
                clock(),
            )?;
            Ok(Flow::On)
        }
        MinterSays::Reimbursed { amount, block } => {
            let fixed = fixed(calls, payout, ck)?;
            let left = subaccount_balance(calls, &fixed).await?;
            // the ledger's block came first: the balance holds it, or a read of it lags
            if left < amount {
                return Ok(Flow::Wait);
            }
            record(
                PoolStep::PayoutCkReimbursed {
                    id,
                    left,
                    block: Some(block),
                    burned: true,
                },
                clock(),
            )?;
            Ok(Flow::On)
        }
        MinterSays::Sent(_) | MinterSays::Nothing => {
            if waited >= ALERT_AFTER {
                log(&format!(
                    "ALERT: the ck payout {id}'s withdrawal {withdrawal_id} has no end yet: a \
                     reimbursement may be quarantined at the minter"
                ));
            }
            Ok(Flow::Wait)
        }
    }
}

/// The ckUSDC of a way back to the main account, all of it less the transfer's fee. Moved
/// already once the subaccount holds less than that, which is read before every ask, so a lost
/// answer never moves it twice and a reimbursement days late is no older than its ask. This
/// never stops: its lines are up and the hourly check leaves the hold out only while it is
/// undoing, so a refusal is an alert and asked again.
async fn bring_back(
    calls: &impl CkCalls,
    payout: &EvmPayout,
    ck: CkLeg,
    clock: impl Fn() -> Timestamp + Copy,
) -> Result<(), Stop> {
    let Some(back) = ck.back.filter(|back| back.left > TokenAmount::ZERO) else {
        return Ok(());
    };
    let fixed = fixed(calls, payout, ck)?;
    if subaccount_balance(calls, &fixed).await? < back.left {
        return Ok(());
    }
    let id = payout.id;
    let amount = ck
        .fee
        .and_then(|fee| back.left.checked_sub(fee))
        .ok_or_else(|| Stop::Failed("the way back does not cover its fee".to_string()))?;
    let dedup = Dedup {
        memo: ck_back_memo(id),
        created_at_nanos: clock().as_nanos(),
    };
    match calls
        .ck_transfer(fixed.ckusdc, Some(fixed.sub), fixed.main, amount, dedup)
        .await
    {
        Ok(_) => Ok(()),
        Err(error) => {
            if !matches!(error, CallError::Unknown(_)) {
                log(&format!("ALERT: the ck payout {id}'s way back: {error}"));
            }
            Err(Stop::Retry(format!("the way back: {error}")))
        }
    }
}

/// Whether payout `id` asks or reads now: at most every `every`, the last time kept in stable
/// memory, and the last times of payouts no longer open dropped.
fn due(id: u64, now: Timestamp, every: Duration) -> bool {
    let open = pool_store::ledger().lines().open;
    timer_state::update(|state| {
        let reads = state.ck_reads.get_or_insert_with(Default::default);
        let kept = reads.len();
        reads.retain(|payout, _| open.contains(payout));
        let dropped = reads.len() != kept;
        let due = reads
            .get(&id)
            .is_none_or(|last| now.saturating_duration_since(*last) >= every);
        if due {
            reads.insert(id, now);
        }
        (due, due || dropped)
    })
}

/// `resolve_ck_withdrawal`'s body, the guards given: what a controller found of a withdrawal
/// the minter leaves open or the canister stopped. A refund stays barred: the way back here is
/// still on the canister's own read of the subaccount.
pub async fn resolve(
    calls: &impl CkCalls,
    halted: bool,
    is_controller: bool,
    id: u64,
    resolution: CkResolution,
    now: Timestamp,
) -> Result<(), PoolError> {
    pool::controller_only(halted, is_controller, "resolves a ck withdrawal")?;
    let payout = pool_store::payout(id).ok_or(PoolError::Unknown)?;
    let ck = payout
        .ck
        .filter(CkLeg::began)
        .ok_or_else(|| refused("the payout has no ck withdrawal"))?;
    match resolution {
        CkResolution::Retry => crate::pool::resolve::record(
            PoolStep::PayoutResolved {
                id,
                resolution: PayoutResolution::Retry,
                refund_at: None,
            },
            now,
        ),
        CkResolution::Delivered { tx_hash } => {
            let tx_hash = <[u8; 32]>::try_from(tx_hash.as_slice())
                .map_err(|_| refused("a transaction hash is 32 bytes"))?;
            crate::pool::resolve::record(
                PoolStep::PayoutCkLanded {
                    id,
                    tx_hash: TxHash::new(tx_hash),
                    by_controller: true,
                },
                now,
            )
        }
        CkResolution::Reimbursed => {
            let fixed = fixed(calls, &payout, ck).map_err(|_| refused("the sums are not fixed"))?;
            let left = calls
                .ck_balance(fixed.ckusdc, fixed.account.clone())
                .await
                .map_err(|error| PoolError::Unavailable(error.to_string()))?;
            if left < fixed.amount {
                return Err(refused(format!(
                    "the subaccount holds {left}, not the {} burned",
                    fixed.amount
                )));
            }
            crate::pool::resolve::record(
                PoolStep::PayoutCkReimbursed {
                    id,
                    left,
                    block: None,
                    burned: true,
                },
                now,
            )
        }
    }
}
