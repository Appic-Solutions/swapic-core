//! `propose_rebalance(move_id, order, gas_limit)` (interface section 3d, spec 3.4): within a
//! move's window the watcher may propose a route for THAT move, which the canister checks as
//! untrusted input, every rule, and takes only when cheaper than any it took before; at the
//! window's end the cheapest taken wins, or the canister's own route goes.
//!
//! The one shape taken: an Eco intent, USDC to USDC, no swap, no fee step, the canister's own
//! order (`payer`, the move's ref, exactly its amount of the source pocket chain's USDC), its
//! route delivering the other chain's USDC to the canister's OWN address (its Ethereum address
//! for a move to ICP, where the canister then makes the ck deposit for its own principal and
//! rebalance subaccount itself; its Base address for a move to Base), every address read from
//! the order's own bytes. The reward's creator is the canister (an unfilled intent refunds it)
//! and its prover Eco's configured one (another prover could pay the reward to anyone); the
//! fee within the cap and below the own route's cost with the canister's own transactions for
//! it; the deadlines short; the gas within the move's row. None for an urgent move, none
//! outside the window: a Base to ICP move's from its opening, an ICP to Base move's from its
//! landing on Ethereum.

#[cfg(test)]
mod tests;

use crate::guards::require_watcher;
use crate::pool::{self, now};
use crate::relay::{read_plan, Chains, MIN_GAS_LIMIT};
use crate::storage::{config, flow_settings};
use settlement_api::types::pool::PoolError;
use types::config::GasAction;
use types::pool::payout::PinnedOrder;
use types::pool::{move_ref, Move, MoveDirection, PoolStep, RebalanceSettings};
use types::swapic::{self, EcoArrival, KnownStep, Order};
use types::{ChainId, EvmAddress, Timestamp, TokenAmount};

/// The latest an order proposed may run: it runs at once, or never.
pub const MAX_ORDER_LIFE_S: u64 = 900;

/// What the checks read besides the order and the move: the source chain the order runs on
/// (its Swapic, its USDC, the chains it may reach), the destination's USDC, Eco's prover, the
/// most gas the move's own transaction takes there, and what the canister's own transactions
/// for an Eco route cost.
pub struct Setup<'a> {
    pub chains: Chains<'a>,
    pub swapic: EvmAddress,
    pub source_usdc: EvmAddress,
    pub destination: ChainId,
    pub destination_usdc: EvmAddress,
    pub prover: Option<EvmAddress>,
    pub max_gas: u64,
    /// The canister's run of the order, and a move to ICP's ck deposit after the fill.
    pub network: TokenAmount,
}

fn refused(why: impl Into<String>) -> PoolError {
    PoolError::NotAllowed(why.into())
}

/// Every check of an order proposed for `item` at `now`, pure: answers the order to pin and
/// what Eco keeps of the move.
#[allow(clippy::too_many_arguments)]
pub fn check(
    item: &Move,
    order: &Order,
    order_bytes: &[u8],
    gas_limit: u64,
    setup: &Setup,
    settings: &RebalanceSettings,
    now: Timestamp,
) -> Result<(PinnedOrder, TokenAmount), PoolError> {
    if !item.proposable() {
        return Err(refused(format!(
            "move {} takes no proposal now: it is {:?}{}",
            item.id,
            item.status,
            if item.urgent { ", urgent" } else { "" }
        )));
    }
    match item.window_ends {
        Some(end) if now < end => {}
        _ => return Err(refused("the move's window is over")),
    }
    let canister = setup.chains.canister;
    if swapic::evm(order.payer) != canister {
        return Err(refused("the order's payer is not the canister"));
    }
    if swapic::evm(order.tokenIn) != setup.source_usdc {
        return Err(refused("the order does not spend the source pocket's USDC"));
    }
    if order.amountIn != item.amount.to_u256() {
        return Err(refused(format!(
            "the order spends {}, not the move's {}",
            order.amountIn, item.amount
        )));
    }
    if order.r#ref.0 != move_ref(item.id).into_bytes() {
        return Err(refused("the order's ref is not the move's"));
    }
    if !order.payout.token.is_zero() || !order.payout.recipient.is_zero() {
        return Err(refused("the order pays something out on its own chain"));
    }
    // read as the Runner will run it: every step a shape the canister reads, no fee step
    let plan = read_plan(order, &setup.chains).map_err(|error| refused(error.to_string()))?;
    if !plan.fees.is_empty() {
        return Err(refused(
            "a move's order carries no fee step: the canister pays itself nothing",
        ));
    }
    let [step] = order.steps.as_slice() else {
        return Err(refused(format!(
            "the order has {} steps, not one Eco intent",
            order.steps.len()
        )));
    };
    let source = setup
        .chains
        .evm_chains
        .get(setup.chains.chain_id)
        .ok_or_else(|| refused("the source chain signs nothing"))?;
    let (intent, fee) = match swapic::read_step(step, source.usdc, source.token_messenger) {
        Ok(KnownStep::Eco { intent, fee }) => (intent, TokenAmount::from_u256(fee)),
        Ok(_) => return Err(refused("the order's step is not an Eco intent")),
        Err(error) => return Err(refused(error.to_string())),
    };
    if setup.chains.eco_portal != Some(swapic::evm(step.target)) {
        return Err(refused("the intent is not published on Eco's Portal"));
    }
    if intent.destination != setup.destination.get() {
        return Err(refused(format!(
            "the intent goes to chain {}, not {}",
            intent.destination, setup.destination
        )));
    }
    if intent.arrival
        != (EcoArrival::Deliver {
            recipient: canister,
        })
    {
        return Err(refused("the intent delivers to another than the canister"));
    }
    if intent.token != setup.destination_usdc {
        return Err(refused(
            "the intent carries another token than the USDC there",
        ));
    }
    if intent.reward_token != setup.source_usdc {
        return Err(refused("the intent's reward is another token"));
    }
    if intent.creator != canister {
        return Err(refused(
            "the intent's creator is not the canister: an unfilled one would refund another",
        ));
    }
    let cap = settings.proposal_fee_cap(item.amount);
    if fee > cap {
        return Err(refused(format!("Eco keeps {fee}, above the cap of {cap}")));
    }
    let cost = fee.checked_add(setup.network).unwrap_or(TokenAmount::MAX);
    if cost >= item.own_cost {
        return Err(refused(format!(
            "Eco's {fee} and the canister's own transactions cost {cost}, not below the own route's {}",
            item.own_cost
        )));
    }
    let now_s = now.as_secs().get();
    let deadline = u64::try_from(order.deadline).unwrap_or(u64::MAX);
    if deadline < now_s.saturating_add(pool::pay::ORDER_MARGIN_S)
        || deadline > now_s.saturating_add(MAX_ORDER_LIFE_S)
    {
        return Err(refused(format!(
            "the order's deadline {deadline} is not within {} s",
            MAX_ORDER_LIFE_S
        )));
    }
    // Eco's prover and Portal, short deadlines: the rules every intent the canister funds
    // meets (`pool::eco`)
    crate::pool::eco::check_intent(
        &intent,
        setup.chains.eco_portal,
        setup.prover,
        deadline,
        now_s,
    )
    .map_err(refused)?;
    if !(MIN_GAS_LIMIT..=setup.max_gas).contains(&gas_limit) {
        return Err(refused(format!(
            "a gas limit of {gas_limit} is not between {MIN_GAS_LIMIT} and {}",
            setup.max_gas
        )));
    }
    let pinned = PinnedOrder {
        order_id: swapic::order_id(order, setup.chains.chain_id, setup.swapic).into_bytes(),
        order: order_bytes.to_vec(),
        gas_limit,
        deadline_s: deadline,
    };
    Ok((pinned, fee))
}

/// `propose_rebalance`'s body.
pub async fn propose(id: u64, order_bytes: Vec<u8>, gas_limit: u64) -> Result<(), PoolError> {
    require_watcher().map_err(pool::guard_error)?;
    pool::require_running()?;
    pool::require_pool_open()?;
    let order =
        swapic::decode_order(&order_bytes).ok_or_else(|| refused("not abi.encode of an Order"))?;
    let canister = crate::ecdsa::canister_address()
        .await
        .map_err(|error| PoolError::Unavailable(error.to_string()))?;
    // after the address's await: the move and the clock as they are now
    let item = super::item(id).ok_or(PoolError::Unknown)?;
    let config = config::get();
    let pool_chain = config
        .pool_chain
        .ok_or_else(|| PoolError::Closed("no pool chain is configured".to_string()))?;
    let (src, dst) = super::cost::chains(item.direction, pool_chain);
    let chain = |id: ChainId| {
        config
            .evm_chains
            .get(id)
            .ok_or_else(|| refused(format!("chain {id} signs nothing")))
    };
    let (source, destination) = (chain(src)?, chain(dst)?);
    let network_fee = |chain_id, action| super::cost::network_fee(&config, chain_id, action);
    let mut network = network_fee(src, GasAction::PoolMove);
    if item.direction == MoveDirection::BaseToIcp {
        network = network
            .checked_add(network_fee(dst, GasAction::PoolCkDeposit))
            .unwrap_or(TokenAmount::MAX);
    }
    let max_gas = source
        .gas_limit
        .get(GasAction::PoolMove)
        .and_then(|gas| gas.try_into_u128())
        .and_then(|gas| u64::try_from(gas).ok())
        .ok_or_else(|| refused(format!("chain {src} has no pool_move gas row")))?;
    let setup = Setup {
        chains: Chains {
            chain_id: src,
            evm_chains: &config.evm_chains,
            domains: &config.cctp_domains,
            canister,
            eco_portal: config.eco_portal,
        },
        swapic: source.swapic,
        source_usdc: source.usdc,
        destination: dst,
        destination_usdc: destination.usdc,
        prover: flow_settings::get().eco_prover,
        max_gas,
        network,
    };
    let settings = crate::storage::events::read_state(super::settings_in);
    let (pinned, fee) = check(
        &item,
        &order,
        &order_bytes,
        gas_limit,
        &setup,
        &settings,
        now(),
    )?;
    // the fold takes it only when cheaper than the one taken
    crate::storage::pool::record(PoolStep::MoveProposed {
        id,
        order: pinned,
        fee,
    })
    .map(drop)
    .map_err(|error| refused(error.to_string()))
}
