//! `swap_from_icp` (spec 3.2, the fixed interface sections 3b and 4): an ICP user swaps
//! ckUSDC or a listed ICP token to any token on an EVM chain through the pool. The canister
//! pulls what the user approved, brings a token to ckUSDC through its route's leg or legs
//! (directly, or through ICP), and only then knows the exact ckUSDC `u`: the payout from the
//! Base pocket is `u - premium - network_fee`, its order built at that exact amount (P13b)
//! and signed by `pay_payout`.
//!
//! Each line before the call it guards:
//! - `PayoutOpened` with the undo account and the pull's dedup pair (the payout's own
//!   instant, memo its ref), then `icrc2_transfer_from`: refused, nothing moved, the payout is
//!   undone; an answer lost, asked again by the payout timer with the same pair.
//! - `PayoutPulled` with the legs' path; each leg recorded before its call
//!   ([`crate::pool::legs`]). `min_usdc` is the ckUSDC the canister holds after the last
//!   leg's output is withdrawn (one ckUSDC fee, interface section 4): a leg that cannot give
//!   it refunds the token, all of it less the fees spent, to `refund_to` (a route through ICP
//!   whose second leg cannot swaps the ICP back into the token first; a way back that fails
//!   stops for a human).
//! - `PayoutAmountKnown`, then `PayoutWaiting` (the Base pocket covers it behind an empty
//!   queue) or `PayoutQueued`.

#[cfg(test)]
mod tests;

use crate::ic::{CallError, Dedup};
use crate::pool::legs::{self, LegEnd, LegSpec};
use crate::pool::payout::{call_stop, open, Ledgers, Leg, Stop};
use crate::pool::{self, now};
use crate::state::{State, Store};
use crate::storage::events::read_state;
use crate::storage::pool as pool_store;
use alloy_primitives::keccak256;
use candid::Principal;
use settlement_api::types::pool::{nat, PoolError};
use std::time::Duration;
use types::pool::payout::{EvmPayout, IcpSwapTerms, PayoutKind, PayoutStatus, PayoutTerms, Undo};
use types::pool::{
    evm_payout_ref, least_premium, IcpAccount, PayoutAttempt, PayoutPath, PoolStep, PrincipalBytes,
    RoutePath, LEG_BACK, LEG_SECOND,
};
use types::{Address, EvmAddress, Timestamp, TokenAmount};

/// The least time a payout's deadline must be ahead of its opening.
pub const MIN_DEADLINE: Duration = Duration::from_secs(300);

/// The swap as its caller asks it, read off the wire.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SwapRequest {
    pub caller: Principal,
    pub terms: IcpSwapTerms,
    pub payout: PayoutTerms,
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
    Ok(SwapRequest {
        caller,
        terms: IcpSwapTerms {
            token_in: args.token_in,
            amount_in: amount("amount_in", &args.amount_in)?,
            from,
            min_usdc: amount("min_usdc", &args.min_usdc)?,
            premium: amount("premium", &args.premium)?,
            network_fee: amount("network_fee", &args.network_fee)?,
            refund_to,
        },
        payout: args.payout.try_into()?,
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

/// Every rule a swap meets before anything is recorded, in the interface's order: the pool's
/// and the payouts' settings, the pool open for swaps, a listed token, no sanctioned party,
/// the premium's floor (per ICPSwap leg of its route), the network fee's floor, the per swap
/// cap, a deadline at least five minutes away, and payout terms the canister signs for.
/// Answers the token's path.
pub fn check<S: Store>(
    state: &State<S>,
    config: &types::Config,
    request: &SwapRequest,
    now: Timestamp,
    canister: Option<EvmAddress>,
    sanctioned: impl Fn(&Address) -> bool,
) -> Result<RoutePath, PoolError> {
    let ledger = state.pool();
    let settings = ledger
        .settings
        .as_ref()
        .ok_or_else(|| PoolError::Closed("the pool has no settings".to_string()))?;
    let payouts = ledger
        .lines()
        .settings
        .ok_or_else(|| PoolError::Closed("the payouts have no settings".to_string()))?;
    if !settings.open_for_swaps {
        return Err(PoolError::Closed(
            "the pool is not open for swaps".to_string(),
        ));
    }
    let terms = &request.terms;
    let path = settings
        .path_of(terms.token_in)
        .ok_or(PoolError::TokenNotListed(terms.token_in))?;
    if terms.amount_in == TokenAmount::ZERO || terms.min_usdc == TokenAmount::ZERO {
        return Err(PoolError::NotAllowed(
            "amount_in and min_usdc must be above zero".to_string(),
        ));
    }
    if path == RoutePath::Ckusdc && terms.min_usdc != terms.amount_in {
        return Err(PoolError::NotAllowed(
            "min_usdc of a ckUSDC swap is its amount_in".to_string(),
        ));
    }
    if screened(request.caller, &sanctioned) || screened(terms.refund_to.owner, &sanctioned) {
        return Err(PoolError::Sanctioned(
            "a party to the swap is sanctioned".to_string(),
        ));
    }
    let least = least_premium(settings, terms.min_usdc, terms.token_in)
        .ok_or_else(|| PoolError::Closed("no ckUSDC fee was read".to_string()))?;
    if terms.premium < least {
        return Err(PoolError::PremiumTooLow { least: nat(least) });
    }
    if terms.network_fee < payouts.min_network_fee {
        return Err(PoolError::NotAllowed(format!(
            "the network fee must be at least {}",
            payouts.min_network_fee
        )));
    }
    if terms.min_usdc > settings.per_swap_cap {
        return Err(PoolError::AboveSwapCap {
            cap: nat(settings.per_swap_cap),
        });
    }
    let fees = terms
        .premium
        .checked_add(terms.network_fee)
        .unwrap_or(TokenAmount::MAX);
    if fees >= terms.min_usdc {
        return Err(PoolError::NotAllowed(
            "the premium and the network fee take all of min_usdc".to_string(),
        ));
    }
    let earliest = now
        .checked_add(MIN_DEADLINE)
        .unwrap_or(Timestamp::from_nanos(u64::MAX));
    if request.payout.deadline < earliest {
        return Err(PoolError::Expired);
    }
    check_payout_terms(config, &request.payout, canister, &sanctioned)?;
    Ok(path)
}

/// `swap_from_icp`'s body: the payout's id. A pull the ledger refused answers its refusal
/// (nothing moved, the payout undone); anything else answers the id, and `payout(id)` tells
/// where it stands (a pull whose answer was lost, or a leg still running, continues on the
/// canister's payout timer).
pub async fn swap_from_icp(
    args: settlement_api::types::pool::IcpSwapTerms,
    sanctioned: impl Fn(&Address) -> bool,
) -> Result<u64, PoolError> {
    pool::require_running()?;
    pool::require_pool_open()?;
    let request = request_of(ic_cdk::api::caller(), args)?;
    let config = crate::storage::config::get();
    let canister = crate::storage::ecdsa_address::get();
    let at = now();
    let id = read_state(|state| -> Result<u64, PoolError> {
        check(state, &config, &request, at, canister, &sanctioned)?;
        Ok(state.pool().lines().next_id)
    })?;
    pool_store::record_at(
        PoolStep::PayoutOpened {
            id,
            kind: Box::new(PayoutKind::IcpSwap(request.terms)),
            terms: request.payout,
            undo: Undo::Refund {
                to: request.terms.refund_to,
            },
        },
        at,
    )
    .map_err(|error| PoolError::NotAllowed(error.to_string()))?;
    if let Some(_busy) = crate::pool::payout::Busy::take(evm_payout_ref(id).into_bytes()) {
        // whatever stops it, the payout timer carries it on
        let _ = drive(
            &crate::pool::payout::Live,
            id,
            now,
            &crate::pool::payout::guard,
        )
        .await;
    }
    match pool_store::payout(id) {
        // the pull was refused: nothing moved
        Some(EvmPayout {
            status: PayoutStatus::Undone(why),
            icp_block: None,
            ..
        }) => Err(PoolError::Ledger(why)),
        _ => Ok(id),
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

/// An ICP swap's next steps, as far as they go now: the pull, the legs, the amount, its place
/// in the queue, and an undo's refund.
pub async fn drive(
    ledgers: &impl Ledgers,
    id: u64,
    clock: impl Fn() -> Timestamp + Copy,
    guard: &impl Fn() -> Result<(), String>,
) -> Result<(), Stop> {
    // a swap goes a few lines at most in one drive: pulled, the legs, known, placed
    for _ in 0..6 {
        let Some(payout) = pool_store::payout(id) else {
            return Ok(());
        };
        // a payout whose amount is known takes its place, whatever it is
        if payout.status == PayoutStatus::Known {
            return place(id, clock());
        }
        if !payout.is_icp_swap() {
            return Ok(());
        }
        match &payout.status {
            PayoutStatus::Opened => pull(ledgers, &payout, clock).await?,
            PayoutStatus::Swapping => swap(ledgers, &payout, clock, guard).await?,
            PayoutStatus::Known => return place(id, clock()),
            PayoutStatus::Undoing(_) => return refund(ledgers, &payout, clock, guard).await,
            _ => return Ok(()),
        }
    }
    Ok(())
}

/// Waiting when the Base pocket covers it behind an empty queue, else queued.
pub fn place(id: u64, now: Timestamp) -> Result<(), Stop> {
    let ledger = pool_store::ledger();
    let Some(payout) = pool_store::payout(id) else {
        return Ok(());
    };
    let covered = ledger.lines().queue_len == 0 && payout.base_hold() <= ledger.pockets.base.free;
    if covered {
        record(PoolStep::PayoutWaiting { id }, now)
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
        let amount = received
            .checked_sub(terms.premium)
            .and_then(|left| left.checked_sub(terms.network_fee))
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
