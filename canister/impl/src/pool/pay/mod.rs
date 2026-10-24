//! `payouts_waiting` and `pay_payout` (the fixed interface section 3b): the watcher asks the
//! quoter for a waiting payout's order at its exact amount and hands it to the canister,
//! which checks the untrusted order against the user's own terms and signs `run(order)` on
//! the pool chain itself, from its own Base pocket, against its standing allowance to Swapic.
//!
//! The checks, every one (a failed one refuses with its reason, and the payout keeps waiting):
//! the payer is the canister; `tokenIn` is the pool chain's USDC; `amountIn` is the payout's
//! exact amount; the order is read as the Runner will run it (the relay's own reader), pays
//! the canister NO fee step (the canister pays itself nothing: a fee step would be booked as
//! fee income and the books would drift), and has exactly one final delivery (its payout on
//! the pool chain, one Eco intent whose unfilled reward refunds its creator, the canister, or
//! from P11c on one CCTP burn, whose finish the canister pays itself: `cctp::own`, taken only
//! for a payout whose network fee is at least `cctp_min_network_fee`: the user paid the
//! canister's finish), which pays the terms' recipient at least their minimum of their token
//! on their chain (a CCTP Deliver: what the burn is given less the most Circle may keep); its
//! deadline is at least 60 s away and at most the payout's; no party is sanctioned; the gas
//! limit is at most `max_payout_gas`.
//! A payout signed once answers its transaction's id, and is never signed again: a revert
//! undoes it (2026-10-23).

#[cfg(test)]
mod tests;

use crate::ecdsa;
use crate::guards::require_watcher;
use crate::pool::{self, now};
use crate::relay::{read_plan, sanctioned_party, Chains, Party, MIN_GAS_LIMIT};
use crate::state::Store;
use crate::storage::events::read_state;
use crate::storage::pool as pool_store;
use crate::tx::{self, Detail};
use alloy_primitives::U256;
use settlement_api::types::pool::{PayoutWaiting, PoolError};
use types::config::GasAction;
use types::events::TxPurpose;
use types::pool::payout::{EvmPayout, PayoutSettings, PayoutStatus, PinnedOrder};
use types::swapic::{self, Order};
use types::{EvmAddress, GasAmount, Timestamp, Wei};

/// The least time an order's deadline must still be ahead of the canister's clock: a
/// signature, the watcher's broadcast and a block, with room.
pub const ORDER_MARGIN_S: u64 = 60;

/// The most payouts `payouts_waiting` lists at once.
pub const MAX_WAITING: usize = 50;

/// What the checks read besides the order: where the canister signs (the pool chain, its
/// Swapic and its USDC) and the chains an order may deliver to.
pub struct Payer<'a> {
    pub chains: Chains<'a>,
    pub swapic: EvmAddress,
    pub usdc: EvmAddress,
    /// Eco's prover, the one a payout's onward intent may name (P11e).
    pub eco_prover: Option<EvmAddress>,
}

/// A refusal of an order or a plan, with why.
pub(crate) fn refused(why: impl Into<String>) -> PoolError {
    PoolError::NotAllowed(why.into())
}

/// What an order's `amountIn` is held to: the payout's exact amount, or nothing yet at a
/// planned swap's opening (2026-10-23), where every other rule holds and the fit checks it
/// again once the amount is known.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AmountIn {
    Exact,
    NotKnownYet,
}

/// Every check of an order handed over for `payout`, pure: answers the order to pin. One
/// validator for the watcher's order, a planned swap's plan at its opening, and the plan
/// fitted to the amount the sale made.
#[allow(clippy::too_many_arguments)]
pub fn check_order(
    payout: &EvmPayout,
    order: &Order,
    order_bytes: &[u8],
    gas_limit: u64,
    payer: &Payer,
    settings: &PayoutSettings,
    now_s: u64,
    sanctioned: impl Fn(&Party) -> bool,
    amount_in: AmountIn,
) -> Result<PinnedOrder, PoolError> {
    let amount = match (amount_in, payout.amount) {
        (AmountIn::Exact, Some(amount)) => Some(amount),
        (AmountIn::Exact, None) => return Err(refused("the payout's amount is not known")),
        (AmountIn::NotKnownYet, None) => None,
        (AmountIn::NotKnownYet, Some(_)) => {
            return Err(refused("the payout's amount is known already"))
        }
    };
    let chain_id = payer.chains.chain_id;
    let order_id = swapic::order_id(order, chain_id, payer.swapic).into_bytes();
    if payout.order.is_some() {
        return Err(refused("a payout is signed once"));
    }
    if swapic::evm(order.payer) != payer.chains.canister {
        return Err(refused("the order's payer is not the canister"));
    }
    if swapic::evm(order.tokenIn) != payer.usdc {
        return Err(refused("the order does not pay in the pool chain's USDC"));
    }
    match amount {
        Some(amount) if order.amountIn != amount.to_u256() => {
            return Err(refused(format!(
                "the order spends {}, not the payout's {amount}",
                order.amountIn
            )));
        }
        None if order.amountIn.is_zero() => return Err(refused("the order spends nothing")),
        _ => {}
    }
    let plan = read_plan(order, &payer.chains).map_err(|error| refused(error.to_string()))?;
    if !plan.fees.is_empty() {
        return Err(refused(
            "a payout order carries no fee step: the canister pays itself nothing",
        ));
    }
    if plan.action == GasAction::RelayCctp {
        // the canister pays a CCTP finish only where the user's network fee paid for one
        settings
            .admits_cctp(payout.network_fee())
            .map_err(refused)?;
    }
    // an onward intent names Eco's own prover and Portal, and short deadlines (P11e): any
    // other prover could pay its reward, the canister's USDC, to whoever it says
    if let Some(intent) = &plan.intent {
        crate::pool::eco::check_intent(
            intent,
            payer.chains.eco_portal,
            payer.eco_prover,
            u64::try_from(order.deadline).unwrap_or(u64::MAX),
            now_s,
        )
        .map_err(refused)?;
    }
    let delivery = plan
        .delivery
        .ok_or_else(|| refused("the order has no single delivery the canister reads"))?;
    let terms = &payout.terms;
    if delivery.chain != terms.dst_chain {
        return Err(refused(format!(
            "the order delivers on chain {}, not {}",
            delivery.chain, terms.dst_chain
        )));
    }
    if delivery.recipient != terms.recipient {
        return Err(refused("the order pays another recipient"));
    }
    if delivery.token != terms.token_out {
        return Err(refused("the order pays another token"));
    }
    if delivery.min_out < terms.min_out {
        return Err(refused(format!(
            "the order guarantees {}, below the {} the recipient asked",
            delivery.min_out, terms.min_out
        )));
    }
    let latest = terms.deadline.as_secs().get();
    let earliest = now_s.saturating_add(ORDER_MARGIN_S);
    if order.deadline < U256::from(earliest) || order.deadline > U256::from(latest) {
        return Err(refused(format!(
            "the order's deadline {} is not between {earliest} and {latest}",
            order.deadline
        )));
    }
    if !(MIN_GAS_LIMIT..=settings.max_payout_gas).contains(&gas_limit) {
        return Err(refused(format!(
            "a gas limit of {gas_limit} is not between {MIN_GAS_LIMIT} and {}",
            settings.max_payout_gas
        )));
    }
    if let Some(party) = plan.parties.iter().copied().find(|party| sanctioned(party)) {
        return Err(PoolError::Sanctioned(format!("{party} is sanctioned")));
    }
    Ok(PinnedOrder {
        order_id,
        order: order_bytes.to_vec(),
        gas_limit,
        deadline_s: u64::try_from(order.deadline).unwrap_or(u64::MAX),
    })
}

/// The payouts waiting for an order, the longest waiting first, at most [`MAX_WAITING`].
pub fn waiting() -> Vec<PayoutWaiting> {
    read_state(|state| {
        let mut waiting: Vec<EvmPayout> = state
            .pool()
            .lines()
            .open
            .into_iter()
            .filter_map(|id| state.store().pool_payout(id))
            .filter(|payout| payout.status == PayoutStatus::Waiting)
            // one being fitted to its plan is the canister's to sign
            .filter(|payout| {
                !crate::pool::payout::Busy::is_held(&types::pool::evm_payout_ref(payout.id))
            })
            .collect();
        waiting.sort_by_key(|payout| (payout.waiting_since, payout.id));
        waiting
            .iter()
            .take(MAX_WAITING)
            .filter_map(PayoutWaiting::of)
            .collect()
    })
}

/// Where a payout stands for a signature: its transaction's id once signed, refused unless
/// waiting, and refused while one is being signed.
fn signable(id: u64) -> Result<Option<u64>, PoolError> {
    let payout = pool_store::payout(id).ok_or(PoolError::Unknown)?;
    match payout.status {
        PayoutStatus::Signed(tx_id) => Ok(Some(tx_id)),
        PayoutStatus::Waiting => {
            let key = TxPurpose::PoolPayout(id)
                .quote_hash()
                .expect("BUG: a pool payout's purpose has a key");
            if read_state(|state| state.store().has_unsigned_nonce_for(&key)) {
                return Err(PoolError::Unavailable(
                    "the payout is being signed".to_string(),
                ));
            }
            Ok(None)
        }
        status => Err(refused(format!("the payout is {status:?}, not waiting"))),
    }
}

/// `pay_payout`'s body: the signed transaction's id in `signed_txs`.
pub async fn pay_payout(id: u64, order_bytes: Vec<u8>, gas_limit: u64) -> Result<u64, PoolError> {
    require_watcher().map_err(pool::guard_error)?;
    pool::require_running()?;
    pool::require_pool_open()?;
    if let Some(tx_id) = signable(id)? {
        return Ok(tx_id);
    }
    let order =
        swapic::decode_order(&order_bytes).ok_or_else(|| refused("not abi.encode of an Order"))?;
    // derived once by the deploy and read from stable memory after that
    let canister = ecdsa::canister_address()
        .await
        .map_err(|error| PoolError::Unavailable(error.to_string()))?;
    // the payout may have moved while the address was read
    if let Some(tx_id) = signable(id)? {
        return Ok(tx_id);
    }
    let pinned = checked(id, &order, &order_bytes, gas_limit, canister, now())?;
    sign(id, &order, pinned).await
}

/// Where the canister signs a payout from, read off `config`: the pool chain, its Swapic and
/// its USDC, and Eco's configured prover.
pub fn payer_of(
    config: &types::Config,
    canister: EvmAddress,
    eco_prover: Option<EvmAddress>,
) -> Result<Payer<'_>, PoolError> {
    let chain_id = config
        .pool_chain
        .ok_or_else(|| PoolError::Closed("no pool chain is configured".to_string()))?;
    let chain = config
        .evm_chains
        .get(chain_id)
        .ok_or_else(|| PoolError::Closed(format!("the pool chain {chain_id} signs nothing")))?;
    Ok(Payer {
        chains: Chains {
            chain_id,
            evm_chains: &config.evm_chains,
            domains: &config.cctp_domains,
            canister,
            eco_portal: config.eco_portal,
        },
        swapic: chain.swapic,
        usdc: chain.usdc,
        eco_prover,
    })
}

/// The checks of an order for payout `id` against the config and the fold as they are now.
pub fn checked(
    id: u64,
    order: &Order,
    order_bytes: &[u8],
    gas_limit: u64,
    canister: EvmAddress,
    now: Timestamp,
) -> Result<PinnedOrder, PoolError> {
    let payout = pool_store::payout(id).ok_or(PoolError::Unknown)?;
    checked_payout(&payout, order, order_bytes, gas_limit, canister, now)
}

/// The checks of an order for `payout`, its amount known, against the config as it is now.
pub fn checked_payout(
    payout: &EvmPayout,
    order: &Order,
    order_bytes: &[u8],
    gas_limit: u64,
    canister: EvmAddress,
    now: Timestamp,
) -> Result<PinnedOrder, PoolError> {
    let config = crate::storage::config::get();
    let settings = pool_store::ledger()
        .lines()
        .settings
        .ok_or_else(|| PoolError::Closed("the payouts have no settings".to_string()))?;
    let payer = payer_of(
        &config,
        canister,
        crate::storage::flow_settings::get().eco_prover,
    )?;
    check_order(
        payout,
        order,
        order_bytes,
        gas_limit,
        &payer,
        &settings,
        now.as_secs().get(),
        |party| sanctioned_party(std::slice::from_ref(party)).is_some(),
        AmountIn::Exact,
    )
}

/// Signs `run(order)` for payout `id`, its order pinned: the signed record is the payout's
/// own `PayoutSigned`, so the payout is signed exactly when its transaction's bytes are
/// recorded. One signature per payout at a time: the fold refuses a second creation while
/// one is unsigned, and a signature unless the payout waits.
pub async fn sign(id: u64, order: &Order, pinned: PinnedOrder) -> Result<u64, PoolError> {
    let busy = crate::pool::payout::Busy::take(types::pool::evm_payout_ref(id).into_bytes())
        .ok_or_else(|| PoolError::Unavailable("the payout is being signed".to_string()))?;
    sign_held(&busy, id, order, pinned).await
}

/// [`sign`] for a caller that holds the payout already (`swap_from_icp`'s drive, the pass):
/// a planned swap's plan, fitted.
pub async fn sign_held(
    _held: &crate::pool::payout::Busy,
    id: u64,
    order: &Order,
    pinned: PinnedOrder,
) -> Result<u64, PoolError> {
    let config = crate::storage::config::get();
    let chain_id = config
        .pool_chain
        .ok_or_else(|| PoolError::Closed("no pool chain is configured".to_string()))?;
    let swapic_address = config
        .evm_chains
        .get(chain_id)
        .map(|chain| chain.swapic)
        .ok_or_else(|| PoolError::Closed(format!("the pool chain {chain_id} signs nothing")))?;
    let gas_limit = GasAmount::from(pinned.gas_limit);
    let signed = tx::create_and_send_with(
        TxPurpose::PoolPayout(id),
        chain_id,
        swapic_address,
        Wei::ZERO,
        swapic::run_calldata(order),
        gas_limit,
        GasAction::PoolPayout,
        Some(Detail::PoolPayout { order: pinned }),
    )
    .await
    .map_err(|error| PoolError::Unavailable(error.to_string()))?;
    Ok(signed.id.get())
}
