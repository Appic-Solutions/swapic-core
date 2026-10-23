//! `swap_from_icp` (spec 3.2, the fixed interface sections 3b and 4; the payout plan of the
//! final review fixes, 2026-10-23): an ICP user swaps ckUSDC or a listed ICP token to any
//! token on an EVM chain through the pool, handing in the quoter's ready-made order for the
//! payout it expects (the payout plan, its `amountIn` the planned payout) and one slippage.
//! The recipient, the token, the chain and the least out are read from the plan, which goes
//! through the same checks as any payout order (`pay::check_order`, its amount not known
//! yet). The canister pulls what the user approved, brings a token to ckUSDC through its
//! route's leg or legs, at least the sale floor (half the slippage below what the plan
//! needs), and only then knows the exact ckUSDC `u`: the payout from the Base pocket is `u`
//! less the settings' premium on it and the payouts' flat network fee. No per swap cap: a
//! swap opens only while the Base pocket can pay it. A plan that fits that payout is signed by
//! the canister in the same message chain ([`crate::pool::plan`]); one that does not waits for
//! the watcher's order (`pay_payout`).
//!
//! Each line before the call it guards:
//! - `PayoutOpened` with the plan, the undo account and the pull's dedup pair (the payout's
//!   own instant, memo its ref), then `icrc2_transfer_from`: refused, nothing moved, the
//!   payout is undone; an answer lost, asked again by the payout timer with the same pair.
//! - `PayoutPulled` with the legs' path; each leg recorded before its call
//!   ([`crate::pool::legs`]). `min_usdc` is the ckUSDC the canister holds after the last
//!   leg's output is withdrawn (one ckUSDC fee, interface section 4): a leg that cannot give
//!   it refunds the token, all of it less the fees spent, to `refund_to` (a route through ICP
//!   whose second leg cannot swaps the ICP back into the token first; a way back that fails
//!   stops for a human).
//! - `PayoutAmountKnown`, then `PayoutWaiting` (the Base pocket covers it behind an empty
//!   queue). A planned swap never queues: one the Base pocket cannot pay any more is undone
//!   at once, its ckUSDC refunded; one opened before the plan flow is queued.
//! - A planned swap's fit, read and checked after those lines: its signature's own lines
//!   (`TxCreated`, `PayoutSigned`) are the caller's, which holds the payout.

#[cfg(test)]
mod tests;

use crate::ic::{CallError, Dedup};
use crate::pool::legs::{self, LegEnd, LegSpec};
use crate::pool::pay::{self, AmountIn};
use crate::pool::payout::{call_stop, open, Busy, Ledgers, Leg, Stop};
use crate::pool::plan::{Fit, Fitted, PlanReads};
use crate::pool::{self, now};
use crate::relay::{read_plan, Party};
use crate::state::{State, Store};
use crate::storage::events::read_state;
use crate::storage::pool as pool_store;
use alloy_primitives::keccak256;
use candid::Principal;
use settlement_api::types::pool::{PoolError, SwapFromIcpOk};
use std::time::Duration;
use types::config::GasAction;
use types::pool::payout::{
    EvmPayout, IcpSwapTerms, PayoutKind, PayoutStatus, PayoutTerms, PinnedOrder, Undo,
};
use types::pool::plan::{self, MAX_SLIPPAGE_BPS};
use types::pool::{
    evm_payout_ref, IcpAccount, PayoutAttempt, PayoutPath, PoolStep, PrincipalBytes, RoutePath,
    LEG_BACK, LEG_SECOND,
};
use types::{Address, EvmAddress, Timestamp, TokenAmount};

/// The least time a payout's deadline must be ahead of its opening.
pub const MIN_DEADLINE: Duration = Duration::from_secs(300);

/// The most time a payout's deadline may be ahead of its opening (Jeff-delegated, 2026-10-23).
pub const MAX_DEADLINE: Duration = Duration::from_secs(3_600);

/// The most bytes a payout plan may have.
pub const MAX_PLAN_BYTES: usize = 16 * 1_024;

/// The swap as its caller asks it, read off the wire.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SwapRequest {
    pub caller: Principal,
    pub token_in: Principal,
    pub amount_in: TokenAmount,
    /// The caller's account the token is pulled from.
    pub from: IcpAccount,
    pub refund_to: IcpAccount,
    pub slippage_bps: u16,
    pub deadline: Timestamp,
    /// The payout plan: `abi.encode` of the order, and its gas.
    pub plan: Vec<u8>,
    pub gas_limit: u64,
}

/// The wire terms as the domain takes them: the caller's account at `from_subaccount` is
/// pulled from, `refund_to` is the caller's account unless named.
pub fn request_of(
    caller: Principal,
    args: settlement_api::types::pool::IcpSwapTerms,
) -> Result<SwapRequest, PoolError> {
    use settlement_api::types::pool::{amount, bytes32};
    let from_subaccount = args
        .from_subaccount
        .map(|bytes| bytes32("from_subaccount", &bytes))
        .transpose()?;
    let from = IcpAccount {
        owner: caller,
        subaccount: from_subaccount,
    };
    let refund_to = match args.refund_to {
        Some(account) => account.try_into()?,
        None => from,
    };
    if args.plan.order.len() > MAX_PLAN_BYTES {
        return Err(PoolError::NotAllowed(format!(
            "a payout plan is at most {MAX_PLAN_BYTES} bytes"
        )));
    }
    Ok(SwapRequest {
        caller,
        token_in: args.token_in,
        amount_in: amount("amount_in", &args.amount_in)?,
        from,
        refund_to,
        slippage_bps: args.slippage_bps,
        deadline: Timestamp::from_nanos(args.deadline_ns),
        plan: args.plan.order,
        gas_limit: args.plan.gas_limit,
    })
}

fn screened(principal: Principal, sanctioned: &impl Fn(&Address) -> bool) -> bool {
    principal
        .to_text()
        .parse::<Address>()
        .map_or(true, |text| sanctioned(&text))
}

/// The payout terms a canister may sign for, whoever asks: a chain it signs on, a token and a
/// minimum, and a recipient that is neither nobody, the canister, nor one of Swapic's own
/// contracts there.
pub fn check_payout_terms(
    config: &types::Config,
    terms: &PayoutTerms,
    canister: Option<EvmAddress>,
    sanctioned: &impl Fn(&Address) -> bool,
) -> Result<(), PoolError> {
    let chain = config.evm_chains.get(terms.dst_chain).ok_or_else(|| {
        PoolError::NotAllowed(format!(
            "chain {} is not one the canister pays on",
            terms.dst_chain
        ))
    })?;
    if terms.min_out == TokenAmount::ZERO {
        return Err(PoolError::NotAllowed(
            "min_out must be above zero".to_string(),
        ));
    }
    if terms.token_out == EvmAddress::ZERO {
        return Err(PoolError::NotAllowed(
            "token_out is the zero address".to_string(),
        ));
    }
    let ours = [
        chain.swapic,
        chain.runner,
        chain.cctp_receiver,
        EvmAddress::ZERO,
    ];
    if ours.contains(&terms.recipient) || Some(terms.recipient) == canister {
        return Err(PoolError::NotAllowed(
            "the recipient is nobody, the canister or one of Swapic's contracts".to_string(),
        ));
    }
    if sanctioned(&Address::from(terms.recipient)) {
        return Err(PoolError::Sanctioned(
            "the recipient is sanctioned".to_string(),
        ));
    }
    Ok(())
}

/// A swap that met every rule: what `PayoutOpened` records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opening {
    /// The token's path into ckUSDC.
    pub path: RoutePath,
    pub terms: IcpSwapTerms,
    /// Read from the plan's one delivery, the deadline the user's.
    pub payout: PayoutTerms,
    pub plan: PinnedOrder,
}

fn refused(why: impl Into<String>) -> PoolError {
    PoolError::NotAllowed(why.into())
}

/// Every rule a swap meets before anything is recorded, in the interface's order: the pool's
/// and the payouts' settings, the pool open for swaps, a listed token, a slippage of at most
/// 500 bps, no sanctioned party, a deadline 5 minutes to 1 hour away; then the plan: an
/// order, its one delivery the payout's terms (a chain the canister pays on, a recipient
/// none of its own), the network fee its rail takes, the sale floor (half the slippage below
/// what the plan's amount needs; a ckUSDC swap sells nothing, its payout exact and within 5%
/// of the plan), the plan's first minimum within what the floor pays, the Base pocket able
/// to pay it behind an empty queue (the plan and 5%, and a ckUSDC swap's own payout), and
/// the plan through the payout orders' own checks, its amount not known yet.
#[allow(clippy::too_many_arguments)]
pub fn check<S: Store>(
    state: &State<S>,
    config: &types::Config,
    request: &SwapRequest,
    now: Timestamp,
    canister: Option<EvmAddress>,
    eco_prover: Option<EvmAddress>,
    sanctioned: impl Fn(&Address) -> bool,
) -> Result<Opening, PoolError> {
    let ledger = state.pool();
    let settings = ledger
        .settings
        .as_ref()
        .ok_or_else(|| PoolError::Closed("the pool has no settings".to_string()))?;
    let lines = ledger.lines();
    let payouts = lines
        .settings
        .ok_or_else(|| PoolError::Closed("the payouts have no settings".to_string()))?;
    if !settings.open_for_swaps {
        return Err(PoolError::Closed(
            "the pool is not open for swaps".to_string(),
        ));
    }
    let path = settings
        .path_of(request.token_in)
        .ok_or(PoolError::TokenNotListed(request.token_in))?;
    if request.amount_in == TokenAmount::ZERO {
        return Err(refused("amount_in must be above zero"));
    }
    if request.slippage_bps > MAX_SLIPPAGE_BPS {
        return Err(refused(format!(
            "the slippage must be at most {MAX_SLIPPAGE_BPS} bps"
        )));
    }
    if screened(request.caller, &sanctioned) || screened(request.refund_to.owner, &sanctioned) {
        return Err(PoolError::Sanctioned(
            "a party to the swap is sanctioned".to_string(),
        ));
    }
    let after = |wait: Duration| {
        now.checked_add(wait)
            .unwrap_or(Timestamp::from_nanos(u64::MAX))
    };
    if request.deadline < after(MIN_DEADLINE) {
        return Err(PoolError::Expired);
    }
    if request.deadline > after(MAX_DEADLINE) {
        return Err(refused("the deadline is more than an hour away"));
    }
    let order = types::swapic::decode_order(&request.plan)
        .ok_or_else(|| refused("the payout plan is not abi.encode of an Order"))?;
    let canister = canister.ok_or_else(|| {
        PoolError::Unavailable("the canister's address is not derived yet".to_string())
    })?;
    let payer = pay::payer_of(config, canister, eco_prover)?;
    let read =
        read_plan(&order, &payer.chains).map_err(|error| refused(format!("the plan {error}")))?;
    let delivery = read
        .delivery
        .ok_or_else(|| refused("the plan has no single delivery the canister reads"))?;
    let payout = PayoutTerms {
        dst_chain: delivery.chain,
        recipient: delivery.recipient,
        token_out: delivery.token,
        min_out: delivery.min_out,
        deadline: request.deadline,
    };
    check_payout_terms(config, &payout, Some(canister), &sanctioned)?;
    // the flat floor of the rail the plan takes (Jeff-delegated): the canister reads no gas
    // price in USDC of its own
    let network_fee = if read.action == GasAction::RelayCctp {
        payouts
            .cctp_min_network_fee
            .ok_or_else(|| refused("no CCTP payout is signed: its least network fee is unset"))?
    } else {
        payouts.min_network_fee
    };
    let planned = TokenAmount::from_be_bytes(order.amountIn.to_be_bytes::<32>());
    let too_large = || refused("the plan's amount is too large");
    let floor = if path == RoutePath::Ckusdc {
        request.amount_in
    } else {
        let expected =
            plan::expected_received(settings, planned, network_fee).ok_or_else(too_large)?;
        plan::sale_floor(expected, request.slippage_bps).ok_or_else(too_large)?
    };
    // what the swap pays at its floor: a ckUSDC swap's exact payout
    let least = plan::payout_of(settings, floor, network_fee)
        .ok_or_else(|| refused("the premium and the network fee take all of the swap"))?;
    if path == RoutePath::Ckusdc && !plan::fits(planned, least) {
        return Err(refused(format!(
            "the plan is built for {planned}, the swap pays {least}: more than 5% apart"
        )));
    }
    let first = plan::first_minimum(&order);
    if first > least {
        return Err(refused(format!(
            "the plan's first step asks {first}, above the {least} the swap pays at its floor"
        )));
    }
    let mut need = plan::open_cover(planned).ok_or_else(too_large)?;
    if path == RoutePath::Ckusdc {
        need = need.max(least);
    }
    if lines.queue_len > 0 || need > ledger.pockets.base.free {
        return Err(refused(format!("the Base pocket cannot pay {need} now")));
    }
    let terms = IcpSwapTerms {
        token_in: request.token_in,
        amount_in: request.amount_in,
        from: request.from,
        min_usdc: floor,
        premium: TokenAmount::ZERO,
        network_fee,
        refund_to: request.refund_to,
        slippage_bps: Some(request.slippage_bps),
        planned: Some(planned),
    };
    let prospective = EvmPayout::new(
        lines.next_id,
        PayoutKind::IcpSwap(terms),
        payout,
        Undo::Refund {
            to: request.refund_to,
        },
        now,
    );
    let pinned = pay::check_order(
        &prospective,
        &order,
        &request.plan,
        request.gas_limit,
        &payer,
        &payouts,
        now.as_secs().get(),
        |party| match party {
            Party::Evm(address) => sanctioned(&Address::from(*address)),
            Party::Icp(principal) => screened(*principal, &sanctioned),
        },
        AmountIn::NotKnownYet,
    )?;
    Ok(Opening {
        path,
        terms,
        payout,
        plan: pinned,
    })
}

/// `swap_from_icp`'s body: the payout's id and where it stands. A pull the ledger refused
/// answers its refusal (nothing moved, the payout undone); anything else answers the id, and
/// `payout(id)` tells where it stands (a pull whose answer was lost, or a leg still running,
/// continues on the canister's payout timer).
pub async fn swap_from_icp(
    args: settlement_api::types::pool::IcpSwapTerms,
    sanctioned: impl Fn(&Address) -> bool,
) -> Result<SwapFromIcpOk, PoolError> {
    pool::require_running()?;
    pool::require_pool_open()?;
    let request = request_of(ic_cdk::api::caller(), args)?;
    let config = crate::storage::config::get();
    let canister = crate::storage::ecdsa_address::get();
    let eco_prover = crate::storage::flow_settings::get().eco_prover;
    let at = now();
    let (id, opening) = read_state(|state| -> Result<(u64, Opening), PoolError> {
        let opening = check(
            state,
            &config,
            &request,
            at,
            canister,
            eco_prover,
            &sanctioned,
        )?;
        Ok((state.pool().lines().next_id, opening))
    })?;
    pool_store::record_at(
        PoolStep::PayoutOpened {
            id,
            kind: Box::new(PayoutKind::IcpSwap(opening.terms)),
            terms: opening.payout,
            undo: Undo::Refund {
                to: opening.terms.refund_to,
            },
            plan: Some(Box::new(opening.plan)),
        },
        at,
    )
    .map_err(|error| PoolError::NotAllowed(error.to_string()))?;
    if let Some(busy) = Busy::take(evm_payout_ref(id).into_bytes()) {
        // whatever stops it, the payout timer carries it on; a plan that fits is signed in
        // this same message chain, and one that is not waits for the watcher's order
        if let Ok(Some(fitted)) = drive(
            &crate::pool::payout::Live,
            id,
            now,
            &crate::pool::payout::guard,
        )
        .await
        {
            if let Err(error) = pay::sign_held(&busy, id, &fitted.order, fitted.pinned).await {
                pool::log(&format!(
                    "the pool payout {id}'s plan was not signed ({error:?}): it waits for an order"
                ));
            }
        }
    }
    match pool_store::payout(id) {
        // the pull was refused: nothing moved
        Some(EvmPayout {
            status: PayoutStatus::Undone(why),
            icp_block: None,
            ..
        }) => Err(PoolError::Ledger(why)),
        Some(payout) => Ok(SwapFromIcpOk {
            id,
            status: (&payout.status).into(),
        }),
        None => Err(PoolError::Unknown),
    }
}

/// The pull's and the refund's dedup pairs: the payout's own instant (and the undo's), memos
/// apart.
fn pull_dedup(payout: &EvmPayout) -> Dedup {
    Dedup {
        memo: evm_payout_ref(payout.id).into_bytes(),
        created_at_nanos: payout.opened_at.as_nanos(),
    }
}

fn refund_dedup(payout: &EvmPayout) -> Dedup {
    let mut preimage = b"swapic.pool.v1.refund".to_vec();
    preimage.extend_from_slice(&payout.id.to_be_bytes());
    Dedup {
        memo: keccak256(&preimage).0,
        created_at_nanos: payout.undoing_at.unwrap_or(payout.opened_at).as_nanos(),
    }
}

fn record(step: PoolStep, now: Timestamp) -> Result<(), Stop> {
    pool_store::record_at(step, now)
        .map(drop)
        .map_err(|error| Stop::Append(error.to_string()))
}

fn terms_of(payout: &EvmPayout) -> Result<IcpSwapTerms, Stop> {
    match payout.kind {
        PayoutKind::IcpSwap(terms) => Ok(terms),
        PayoutKind::Withdrawal(_) => Err(Stop::Failed("a withdrawal has no ICP leg".to_string())),
    }
}

/// An ICP swap's next steps, as far as they go now: the pull, the legs, the amount, its place,
/// a planned swap's fit (the order to sign, answered to the caller, which holds the payout),
/// and an undo's refund.
pub async fn drive(
    ledgers: &(impl Ledgers + PlanReads),
    id: u64,
    clock: impl Fn() -> Timestamp + Copy,
    guard: &impl Fn() -> Result<(), String>,
) -> Result<Option<Fitted>, Stop> {
    let mut placed = false;
    // a swap goes a few lines at most in one drive: pulled, the legs, known, placed, undone
    for _ in 0..6 {
        let Some(payout) = pool_store::payout(id) else {
            return Ok(None);
        };
        // a payout whose amount is known takes its place, whatever it is: a planned swap the
        // Base pocket cannot pay any more is undone, its refund next
        if payout.status == PayoutStatus::Known {
            place(id, clock())?;
            placed = true;
            continue;
        }
        // a planned swap placed by this drive: its plan fitted to its exact amount, once
        if placed && payout.status == PayoutStatus::Waiting && is_planned(&payout) {
            placed = false;
            match crate::pool::plan::fit(ledgers, &payout, clock()).await {
                Fit::Sign(fitted) => return Ok(open(guard).is_ok().then_some(*fitted)),
                Fit::Wait(why) => {
                    pool::log(&format!(
                        "the pool payout {id} waits for an order: its plan {why}"
                    ));
                    return Ok(None);
                }
                // it could only revert: the user's ckUSDC back at once
                Fit::Undo(why) => {
                    undo(id, why, clock())?;
                    continue;
                }
            }
        }
        if !payout.is_icp_swap() {
            return Ok(None);
        }
        match &payout.status {
            PayoutStatus::Opened => pull(ledgers, &payout, clock).await?,
            PayoutStatus::Swapping => swap(ledgers, &payout, clock, guard).await?,
            PayoutStatus::Undoing(_) => {
                return refund(ledgers, &payout, clock, guard).await.map(|()| None)
            }
            _ => return Ok(None),
        }
    }
    Ok(None)
}

fn is_planned(payout: &EvmPayout) -> bool {
    matches!(payout.kind, PayoutKind::IcpSwap(terms) if terms.is_planned())
}

/// Why a planned swap the Base pocket cannot pay any more is undone.
pub const BASE_SHORT: &str = "the Base pocket cannot pay it now";

/// Waiting when the Base pocket covers it behind an empty queue; else a planned swap is
/// undone at once (it never queues, 2026-10-23), and any other payout queued.
pub fn place(id: u64, now: Timestamp) -> Result<(), Stop> {
    let ledger = pool_store::ledger();
    let Some(payout) = pool_store::payout(id) else {
        return Ok(());
    };
    let covered = ledger.lines().queue_len == 0 && payout.base_hold() <= ledger.pockets.base.free;
    if covered {
        record(PoolStep::PayoutWaiting { id }, now)
    } else if is_planned(&payout) {
        undo(id, BASE_SHORT.to_string(), now)
    } else {
        record(PoolStep::PayoutQueued { id }, now)
    }
}

/// The path the token's legs take into ckUSDC, read the way the token goes in.
fn inbound_path(settings: &types::pool::PoolSettings, token: Principal) -> Option<PayoutPath> {
    match settings.path_of(token)? {
        RoutePath::Ckusdc => None,
        RoutePath::Direct { pool } => Some(PayoutPath::Icpswap { pool }),
        RoutePath::ViaIcp { first, second, icp } => Some(PayoutPath::IcpswapVia {
            first: second,
            second: first,
            icp,
        }),
    }
}

async fn pull(
    ledgers: &impl Ledgers,
    payout: &EvmPayout,
    clock: impl Fn() -> Timestamp + Copy,
) -> Result<(), Stop> {
    let terms = terms_of(payout)?;
    let settings = pool_store::ledger()
        .settings
        .ok_or_else(|| Stop::Retry("the pool has no settings".to_string()))?;
    match ledgers
        .pull(
            terms.token_in,
            terms.from,
            terms.amount_in,
            pull_dedup(payout),
        )
        .await
    {
        Ok(block) => {
            // a token no longer listed has no path: its legs refund it
            let attempt = inbound_path(&settings, terms.token_in).map(|path| PayoutAttempt {
                created_at: payout.opened_at,
                memo: evm_payout_ref(payout.id).into_bytes(),
                path,
                amount: Some(terms.amount_in),
                token: Some(PrincipalBytes(terms.token_in)),
            });
            record(
                PoolStep::PayoutPulled {
                    id: payout.id,
                    block,
                    attempt,
                },
                clock(),
            )
        }
        // nothing moved: the payout ends here
        Err(CallError::Refused(why)) => {
            record(
                PoolStep::PayoutUndone {
                    id: payout.id,
                    why: format!("the ledger refused the pull: {why}"),
                    block: None,
                },
                clock(),
            )?;
            Err(Stop::Failed(format!("the ledger refused the pull: {why}")))
        }
        Err(error @ CallError::Expired(_)) => {
            record(
                PoolStep::PayoutStopped {
                    id: payout.id,
                    why: format!("ALERT: the pull's outcome can no longer be known: {error}"),
                },
                clock(),
            )?;
            Err(Stop::Failed(error.to_string()))
        }
        Err(error @ CallError::Unknown(_)) => Err(Stop::Retry(format!("the pull: {error}"))),
    }
}

fn undo(id: u64, why: String, now: Timestamp) -> Result<(), Stop> {
    record(PoolStep::PayoutUndoing { id, why }, now)
}

/// The ckUSDC the canister holds of a leg's output: the output less the pool's withdrawal.
fn landed(out: TokenAmount, fee: TokenAmount) -> TokenAmount {
    out.checked_sub(fee).unwrap_or(TokenAmount::ZERO)
}

async fn swap(
    ledgers: &impl Ledgers,
    payout: &EvmPayout,
    clock: impl Fn() -> Timestamp + Copy,
    guard: &impl Fn() -> Result<(), String>,
) -> Result<(), Stop> {
    let terms = terms_of(payout)?;
    let settings = pool_store::ledger()
        .settings
        .ok_or_else(|| Stop::Retry("the pool has no settings".to_string()))?;
    let ckusdc = settings.ckusdc_ledger;
    let fee_ck = settings
        .fee_of(ckusdc)
        .ok_or_else(|| Stop::Failed("no ckUSDC fee was read".to_string()))?;
    let known = |received: TokenAmount| -> Result<(), Stop> {
        // a planned swap's premium is the settings' share of what it made (2026-10-23)
        let amount = if terms.is_planned() {
            plan::payout_of(&settings, received, terms.network_fee)
        } else {
            received
                .checked_sub(terms.premium)
                .and_then(|left| left.checked_sub(terms.network_fee))
        }
        .ok_or_else(|| Stop::Failed("the ckUSDC does not cover the fees".to_string()))?;
        record(
            PoolStep::PayoutAmountKnown {
                id: payout.id,
                amount,
                received: Some(received),
            },
            clock(),
        )
    };
    let Some(progress) = payout.legs.clone() else {
        // ckUSDC in: the amount pulled is the ckUSDC; a token no longer listed goes back
        if terms.token_in == ckusdc {
            return known(terms.amount_in);
        }
        return undo(
            payout.id,
            format!("{} is no longer listed", terms.token_in),
            clock(),
        );
    };
    let attempt = progress
        .attempt
        .ok_or_else(|| Stop::Failed("the legs have no path".to_string()))?;
    let fee_in = settings
        .fee_of(terms.token_in)
        .ok_or_else(|| Stop::Failed(format!("no fee was read for {}", terms.token_in)))?;
    // what the user's ckUSDC minimum asks of the last leg's output: the ckUSDC held after its
    // withdrawal to the canister, which takes one fee
    let least = terms
        .min_usdc
        .checked_add(fee_ck)
        .ok_or_else(|| Stop::Failed("min_usdc is too large".to_string()))?;
    // the first leg sells what was pulled less its approval's and its pull's fees
    let in0 = terms
        .amount_in
        .checked_sub(fee_in.checked_mul(2_u8).unwrap_or(TokenAmount::MAX))
        .filter(|left| *left > TokenAmount::ZERO)
        .ok_or_else(|| Stop::Failed("the amount does not cover the token's fees".to_string()))?;
    let id = evm_payout_ref(payout.id);
    match attempt.path {
        PayoutPath::Ckusdc => known(terms.amount_in),
        PayoutPath::Icpswap { pool } => {
            let spec = LegSpec {
                id,
                leg: None,
                pool,
                token_in: terms.token_in,
                token_out: ckusdc,
                amount_in: in0,
                least,
                fee_in,
                fee_out: fee_ck,
                pays_out: false,
            };
            match run_leg(ledgers, spec, progress.swap, clock, guard).await? {
                LegEnd::Out(out) => known(landed(out, fee_ck)),
                LegEnd::Short(quoted) => undo(
                    payout.id,
                    format!("the pool quotes {quoted}, below the {least} min_usdc needs"),
                    clock(),
                ),
                LegEnd::NoPair => undo(
                    payout.id,
                    "the pool does not trade the pair".to_string(),
                    clock(),
                ),
                LegEnd::NotDone(why) => undo(payout.id, why, clock()),
            }
        }
        PayoutPath::IcpswapVia { first, second, icp } => {
            let fee_icp = settings
                .fee_of(icp)
                .ok_or_else(|| Stop::Failed("no fee was read for ICP".to_string()))?;
            let fees_before_second = fee_icp
                .checked_mul(3_u8)
                .ok_or_else(|| Stop::Failed("the ICP fees overflow".to_string()))?;
            let out0 = match progress.swap.out {
                Some(out) => out,
                None => {
                    let mut least0 = TokenAmount::ZERO;
                    if !progress.swap.begun {
                        match chained(
                            ledgers,
                            (first, terms.token_in, in0),
                            (second, icp, ckusdc),
                            fees_before_second,
                            least,
                        )
                        .await?
                        {
                            Ok(at_least) => least0 = at_least,
                            Err(why) => return undo(payout.id, why, clock()),
                        }
                    }
                    let spec = LegSpec {
                        id,
                        leg: None,
                        pool: first,
                        token_in: terms.token_in,
                        token_out: icp,
                        amount_in: in0,
                        least: least0,
                        fee_in,
                        fee_out: fee_icp,
                        pays_out: false,
                    };
                    match run_leg(ledgers, spec, progress.swap, clock, guard).await? {
                        LegEnd::Out(out) => out,
                        LegEnd::Short(quoted) => {
                            return undo(
                                payout.id,
                                format!("the token's ICP pool quotes {quoted}, below {least0}"),
                                clock(),
                            )
                        }
                        LegEnd::NoPair => {
                            return undo(
                                payout.id,
                                "the token's ICP pool does not trade the pair".to_string(),
                                clock(),
                            )
                        }
                        LegEnd::NotDone(why) => return undo(payout.id, why, clock()),
                    }
                }
            };
            let progress = pool_store::payout(payout.id)
                .and_then(|payout| payout.legs)
                .unwrap_or(progress);
            let second_leg = progress.leg(Some(LEG_SECOND));
            let back = progress.leg(Some(LEG_BACK));
            let in1 = out0
                .checked_sub(fees_before_second)
                .unwrap_or(TokenAmount::ZERO);
            if !back.begun && back.out.is_none() && in1 > TokenAmount::ZERO {
                let spec = LegSpec {
                    id,
                    leg: Some(LEG_SECOND),
                    pool: second,
                    token_in: icp,
                    token_out: ckusdc,
                    amount_in: in1,
                    least,
                    fee_in: fee_icp,
                    fee_out: fee_ck,
                    pays_out: false,
                };
                if let LegEnd::Out(out) = run_leg(ledgers, spec, second_leg, clock, guard).await? {
                    return known(landed(out, fee_ck));
                }
            }
            // the second leg cannot give min_usdc: the ICP goes back into the token
            let progress = pool_store::payout(payout.id)
                .and_then(|payout| payout.legs)
                .unwrap_or(progress);
            let second_leg = progress.leg(Some(LEG_SECOND));
            let held = out0
                .checked_sub(fee_icp)
                .and_then(|left| left.checked_sub(legs::cost_in(&second_leg, fee_icp)?))
                .unwrap_or(TokenAmount::ZERO);
            let amount_back = held
                .checked_sub(fee_icp.checked_mul(2_u8).unwrap_or(TokenAmount::MAX))
                .unwrap_or(TokenAmount::ZERO);
            if amount_back == TokenAmount::ZERO {
                return stop(
                    payout.id,
                    "ALERT: the ICP the first leg bought does not cover the way back",
                    clock(),
                );
            }
            let spec = LegSpec {
                id,
                leg: Some(LEG_BACK),
                pool: first,
                token_in: icp,
                token_out: terms.token_in,
                amount_in: amount_back,
                least: TokenAmount::ONE,
                fee_in: fee_icp,
                fee_out: fee_in,
                pays_out: false,
            };
            match run_leg(ledgers, spec, progress.leg(Some(LEG_BACK)), clock, guard).await? {
                LegEnd::Out(_) => undo(
                    payout.id,
                    "the second leg could not give min_usdc; the ICP went back into the token"
                        .to_string(),
                    clock(),
                ),
                LegEnd::Short(_) | LegEnd::NoPair | LegEnd::NotDone(_) => stop(
                    payout.id,
                    "ALERT: the ICP could not be swapped back into the token; the swap waits for a human",
                    clock(),
                ),
            }
        }
    }
}

fn stop(id: u64, why: &str, now: Timestamp) -> Result<(), Stop> {
    record(
        PoolStep::PayoutStopped {
            id,
            why: why.to_string(),
        },
        now,
    )?;
    Err(Stop::Failed(why.to_string()))
}

/// One leg, its pool held for it alone and no other leg there unsettled.
async fn run_leg(
    ledgers: &impl Ledgers,
    spec: LegSpec,
    state: types::pool::SwapLeg,
    clock: impl Fn() -> Timestamp + Copy,
    guard: &impl Fn() -> Result<(), String>,
) -> Result<LegEnd, Stop> {
    if state.out.is_some() {
        return legs::run(ledgers, spec, state, clock, guard).await;
    }
    let Some(_held) = Leg::take(spec.pool) else {
        return Err(Stop::Retry(format!(
            "another payout's leg is at the ICPSwap pool {}",
            spec.pool
        )));
    };
    if let Some(other) =
        read_state(|state| crate::pool::payout::unsettled_at(state, spec.pool, spec.id))
    {
        return Err(Stop::Retry(format!(
            "the swap of {other} at the ICPSwap pool {} is not settled yet",
            spec.pool
        )));
    }
    legs::run(ledgers, spec, state, clock, guard).await
}

/// The least the first of two legs must give, from both pools' quotes: the second leg's quote
/// for what the first buys (less `fees`) must reach `least`; below it, why.
async fn chained(
    ledgers: &impl Ledgers,
    (first, token_in, amount_in): (Principal, Principal, TokenAmount),
    (second, middle, token_out): (Principal, Principal, Principal),
    fees: TokenAmount,
    least: TokenAmount,
) -> Result<Result<TokenAmount, String>, Stop> {
    let Some(zero_for_one) = ledgers
        .direction(first, token_in, middle)
        .await
        .map_err(|error| call_stop("the pool's metadata", error))?
    else {
        return Ok(Err("the first pool does not trade the pair".to_string()));
    };
    let bought = ledgers
        .quote(first, amount_in, zero_for_one)
        .await
        .map_err(|error| call_stop("the pool's quote", error))?;
    let sold = bought.checked_sub(fees).unwrap_or(TokenAmount::ZERO);
    let Some(zero_for_one) = ledgers
        .direction(second, middle, token_out)
        .await
        .map_err(|error| call_stop("the pool's metadata", error))?
    else {
        return Ok(Err("the second pool does not trade the pair".to_string()));
    };
    let quoted = ledgers
        .quote(second, sold, zero_for_one)
        .await
        .map_err(|error| call_stop("the pool's quote", error))?;
    if sold == TokenAmount::ZERO || quoted < least {
        return Ok(Err(format!(
            "the pools quote {quoted} through ICP, below the {least} min_usdc needs"
        )));
    }
    legs::first_leg_least(sold, quoted, least, fees)
        .map(Ok)
        .ok_or_else(|| Stop::Failed("the first leg's minimum overflows".to_string()))
}

/// What an undone swap gives back, and in which token: its ckUSDC (all it made, less the
/// ledger's fee) once known; else the token, all of it less what its legs spent and the
/// transfer's fee (a way back from ICP adds what it bought).
pub fn refund_of(
    payout: &EvmPayout,
    settings: &types::pool::PoolSettings,
) -> Option<(Principal, TokenAmount)> {
    let PayoutKind::IcpSwap(terms) = payout.kind else {
        return None;
    };
    if let Some(received) = payout.received {
        let fee = settings.fee_of(settings.ckusdc_ledger)?;
        return Some((
            settings.ckusdc_ledger,
            received
                .checked_sub(fee)
                .filter(|left| *left > TokenAmount::ZERO)?,
        ));
    }
    let fee = settings.fee_of(terms.token_in)?;
    let progress = payout.legs.clone().unwrap_or_default();
    let spent = legs::cost_in(&progress.swap, fee)?;
    let back = progress
        .back
        .and_then(|back| back.out)
        .map_or(TokenAmount::ZERO, |out| landed(out, fee));
    let held = terms.amount_in.checked_sub(spent)?.checked_add(back)?;
    Some((
        terms.token_in,
        held.checked_sub(fee)
            .filter(|left| *left > TokenAmount::ZERO)?,
    ))
}

/// An undo's refund to `refund_to`, with its own dedup pair, then `PayoutUndone`. A refusal
/// stops it for a human; an answer lost is asked again as the same transfer.
async fn refund(
    ledgers: &impl Ledgers,
    payout: &EvmPayout,
    clock: impl Fn() -> Timestamp + Copy,
    guard: &impl Fn() -> Result<(), String>,
) -> Result<(), Stop> {
    let PayoutStatus::Undoing(why) = &payout.status else {
        return Ok(());
    };
    let Undo::Refund { to } = payout.undo else {
        return Err(Stop::Failed("an ICP swap's undo is a refund".to_string()));
    };
    let settings = pool_store::ledger()
        .settings
        .ok_or_else(|| Stop::Retry("the pool has no settings".to_string()))?;
    let Some((ledger, amount)) = refund_of(payout, &settings) else {
        return stop(
            payout.id,
            "ALERT: the refund does not cover its ledger's fee",
            clock(),
        );
    };
    open(guard)?;
    match ledgers
        .transfer(ledger, to, amount, refund_dedup(payout))
        .await
    {
        Ok(block) => record(
            PoolStep::PayoutUndone {
                id: payout.id,
                why: why.clone(),
                block: Some(block),
            },
            clock(),
        ),
        Err(error @ CallError::Unknown(_)) => Err(Stop::Retry(format!("the refund: {error}"))),
        Err(error) => stop(
            payout.id,
            &format!("ALERT: the refund was refused: {error}"),
            clock(),
        ),
    }
}
