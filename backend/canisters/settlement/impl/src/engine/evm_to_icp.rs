//! EVM to ICP: the ckUSDC that lands in a registered quote's subaccount (spec 4b.8) is
//! swapped on ICPSwap to the token the user wants and paid to the user's principal. The
//! arrival is found by a ledger balance check on the engine's tick: nothing announces it.
//! A pool that would pay below the user's floor is not used, and the user is paid the
//! ckUSDC instead.

use super::flows::{
    amount, dedup, freeze, nat, now, principal, record, swap_terms, FlowDriveError, SwapRefusal,
};
use crate::ic::{self, Account, CallError};
use crate::storage::events::read_state;
use crate::storage::flow_settings;
use candid::Principal;
use types::flow::{EvmToIcpQuote, Stage};
use types::{Flow, FlowStatus, FlowStep, QuoteHash, TokenAmount};

/// The flow's next step.
pub async fn step(quote_hash: QuoteHash, flow: &Flow) -> Result<(), FlowDriveError> {
    let quote = flow
        .evm_quote
        .clone()
        .ok_or(FlowDriveError::Settings("an EVM to ICP flow has no quote"))?;
    let ckusdc = flow_settings::get()
        .ckusdc_ledger
        .ok_or(FlowDriveError::Settings("no ckUSDC ledger"))?;
    let target = principal(&quote.target_ledger).ok_or(FlowDriveError::Settings("target"))?;
    match flow.status {
        FlowStatus::Registered => check_arrival(quote_hash, &quote, ckusdc).await,
        FlowStatus::Arrived if target == ckusdc => deliver(quote_hash, flow, &quote).await,
        FlowStatus::Arrived | FlowStatus::Consolidating => {
            consolidate(quote_hash, flow, ckusdc).await
        }
        FlowStatus::Consolidated => swap(quote_hash, flow, &quote, ckusdc, target).await,
        FlowStatus::Swapping => freeze(quote_hash, "the ICPSwap call's outcome is unknown"),
        FlowStatus::Swapped | FlowStatus::Delivering => deliver(quote_hash, flow, &quote).await,
        _ => Ok(()),
    }
}

/// The ckUSDC in the quote's subaccount, once there is more of it than a transfer costs.
async fn check_arrival(
    quote_hash: QuoteHash,
    quote: &EvmToIcpQuote,
    ckusdc: Principal,
) -> Result<(), FlowDriveError> {
    if now().as_secs() > quote.expires_at {
        record(quote_hash, FlowStep::Expired)?;
        return Ok(());
    }
    let me = ic_cdk::api::id();
    let balance = amount(
        &ic::icrc1_balance_of(ckusdc, Account::of(me, Some(quote_hash.into_bytes()))).await?,
    );
    let fee = amount(&ic::icrc1_fee(ckusdc).await?);
    if balance <= fee {
        return Ok(());
    }
    record(
        quote_hash,
        FlowStep::Arrived {
            ledger: ckusdc.as_slice().to_vec(),
            amount: balance,
        },
    )?;
    Ok(())
}

/// The flow as the fold holds it after `step`, a begun marker the caller just appended.
fn reread(quote_hash: QuoteHash) -> Result<Flow, FlowDriveError> {
    read_state(|state| state.flow(&quote_hash)).ok_or(FlowDriveError::Settings("no flow"))
}

/// Moves the ckUSDC from the quote's subaccount to the canister's own account, where
/// ICPSwap pulls from.
async fn consolidate(
    quote_hash: QuoteHash,
    flow: &Flow,
    ckusdc: Principal,
) -> Result<(), FlowDriveError> {
    let fee = amount(&ic::icrc1_fee(ckusdc).await?);
    let moved = flow
        .held
        .checked_sub(fee)
        .ok_or(FlowDriveError::Settings("below the fee"))?;
    let flow = if flow.status == FlowStatus::Arrived {
        record(quote_hash, FlowStep::Begun { stage: Stage::Consolidate })?;
        reread(quote_hash)?
    } else {
        flow.clone()
    };
    let me = ic_cdk::api::id();
    ic::icrc1_transfer(
        ckusdc,
        Some(quote_hash.into_bytes()),
        Account::of(me, None),
        nat(moved),
        dedup(quote_hash, &flow),
    )
    .await?;
    record(quote_hash, FlowStep::Consolidated { amount: moved })?;
    Ok(())
}

/// ICPSwap from ckUSDC to the target, at no less than the user's floor.
async fn swap(
    quote_hash: QuoteHash,
    flow: &Flow,
    quote: &EvmToIcpQuote,
    ckusdc: Principal,
    target: Principal,
) -> Result<(), FlowDriveError> {
    let skip = |reason: String| record(quote_hash, FlowStep::SwapSkipped { reason });
    let Some(pool) = quote.pool.as_ref().and_then(|bytes| principal(bytes)) else {
        skip("the quote names no pool".to_string())?;
        return Ok(());
    };
    let (zero_for_one, fee_in, fee_out, swap_in) =
        match swap_terms(pool, ckusdc, target, flow.held).await {
            Ok(terms) => terms,
            Err(SwapRefusal::Refund(reason)) => {
                skip(reason)?;
                return Ok(());
            }
            Err(SwapRefusal::Retry(error)) => return Err(error),
        };
    let quoted = ic::pool_quote(pool, &nat(swap_in), zero_for_one).await?;
    if amount(&quoted) < quote.min_out {
        skip(format!(
            "the pool quotes {quoted}, below the user's floor of {}",
            quote.min_out
        ))?;
        return Ok(());
    }
    ic::icrc2_approve(ckusdc, pool, nat(swap_in) + fee_in.clone()).await?;
    record(quote_hash, FlowStep::Begun { stage: Stage::Swap })?;
    match ic::deposit_from_and_swap(
        pool,
        &nat(swap_in),
        zero_for_one,
        &nat(quote.min_out),
        fee_in,
        fee_out,
    )
    .await
    {
        Ok(out) => {
            record(
                quote_hash,
                FlowStep::Swapped {
                    ledger: target.as_slice().to_vec(),
                    amount: amount(&out),
                },
            )?;
            Ok(())
        }
        Err(error) => freeze(quote_hash, &format!("the ICPSwap swap did not answer: {error}")),
    }
}

/// Pays what the flow holds to the user's principal, less the ledger's fee, asked again
/// with the same key while its outcome is in doubt.
async fn deliver(
    quote_hash: QuoteHash,
    flow: &Flow,
    quote: &EvmToIcpQuote,
) -> Result<(), FlowDriveError> {
    let ledger = flow
        .held_ledger()
        .and_then(principal)
        .ok_or(FlowDriveError::Settings("the flow holds no ledger"))?;
    let user = principal(&quote.user).ok_or(FlowDriveError::Settings("user"))?;
    let fee = amount(&ic::icrc1_fee(ledger).await?);
    let Some(paid) = flow
        .held
        .checked_sub(fee)
        .filter(|left| *left > TokenAmount::ZERO)
    else {
        return freeze(quote_hash, "what is held does not cover the transfer fee");
    };
    let flow = if flow.status == FlowStatus::Delivering {
        flow.clone()
    } else {
        record(quote_hash, FlowStep::Begun { stage: Stage::Deliver })?;
        reread(quote_hash)?
    };
    let from = flow.held_in_subaccount.then_some(quote_hash.into_bytes());
    let answer = ic::icrc1_transfer(
        ledger,
        from,
        Account::of(user, None),
        nat(paid),
        dedup(quote_hash, &flow),
    )
    .await;
    match answer {
        Ok(block_index) => {
            record(
                quote_hash,
                FlowStep::Delivered {
                    amount: paid,
                    block_index,
                },
            )?;
            Ok(())
        }
        Err(CallError::Refused(reason)) => {
            freeze(quote_hash, &format!("the ledger refused the payment: {reason}"))
        }
        Err(error) => Err(error.into()),
    }
}
