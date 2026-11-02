//! The open moves, a step each, every minute (the rebalancing's timer): a route decided when
//! the window ends, the pinned order signed (and signed again, the same order, after a
//! revert), the ck minter asked, the landing on Ethereum read, an Eco fill found and its ck
//! deposit made, the rebalance subaccount swept. A move's own transactions close in the
//! outbox (`MoveBurned`, `MoveReverted`); its Base arrival is reported by the watcher
//! (`note_move_arrival`) or found by the canister's own read of Base.
//!
//! Every outflow (a signature, the minter's call) waits while the pool is closed. A move
//! that cannot go on stops for a controller (`MoveStuck`), its lines where they are.

#[cfg(test)]
mod tests;

use super::{cost, own::OwnRoute};
use crate::ic::{self, Account, CallError, Dedup};
use crate::pool::{self, payout::Busy};
use crate::storage::events::read_state;
use crate::storage::{chain_data, config, flow_settings, pool as pool_store};
use crate::tx;
use candid::{Nat, Principal};
use serde_json::{json, Value};
use std::time::Duration;
use types::config::GasAction;
use types::events::TxPurpose;
use types::pool::payout::PinnedOrder;
use types::pool::rebalance::ck_deposit_data;
use types::pool::{
    ck_word, move_ref, rebalance_subaccount, Move, MoveDirection, MoveRoute, MoveStatus, PoolStep,
    RebalanceSettings,
};
use types::swapic;
use types::{BlockNumber, ChainId, EvmAddress, GasAmount, Timestamp, TokenAmount, TxHash, Wei};

/// How long a move may wait in one position before it stops for a controller: the ck
/// minter's 20 minutes and CCTP's standard attestation, many times over.
pub const STUCK_AFTER: Duration = Duration::from_secs(3 * 3_600);

/// How many times a move's order may revert before it stops for a controller.
pub const MAX_REVERTS: u32 = 3;

/// The least time the pinned order's deadline must still be ahead to sign it.
pub const SIGN_MARGIN_S: u64 = 120;

/// The most blocks one read for an Eco fill looks at.
pub const FILL_BLOCKS: u64 = 500;

/// The ICP calls a move's legs make, so the driver runs the same against the ledgers and the
/// minter, and against a fake.
#[allow(async_fn_in_trait)]
pub trait IcpCalls {
    async fn balance(&self, ledger: Principal, account: Account) -> Result<Nat, CallError>;
    async fn approve(
        &self,
        ledger: Principal,
        spender: Principal,
        amount: Nat,
        dedup: Dedup,
    ) -> Result<(), CallError>;
    async fn transfer(
        &self,
        ledger: Principal,
        from_subaccount: Option<[u8; 32]>,
        to: Account,
        amount: Nat,
        dedup: Dedup,
    ) -> Result<u64, CallError>;
    async fn withdraw_erc20(
        &self,
        minter: Principal,
        ledger: Principal,
        amount: Nat,
        recipient: String,
    ) -> Result<ic::RetrieveErc20Request, CallError>;
    async fn withdrawal_status(
        &self,
        minter: Principal,
        id: u64,
    ) -> Result<Vec<ic::WithdrawalDetail>, CallError>;
}

/// The real ledgers and minter.
pub struct Live;

impl IcpCalls for Live {
    async fn balance(&self, ledger: Principal, account: Account) -> Result<Nat, CallError> {
        ic::icrc1_balance_of(ledger, account).await
    }

    async fn approve(
        &self,
        ledger: Principal,
        spender: Principal,
        amount: Nat,
        dedup: Dedup,
    ) -> Result<(), CallError> {
        ic::icrc2_approve_once(ledger, spender, amount, dedup).await
    }

    async fn transfer(
        &self,
        ledger: Principal,
        from_subaccount: Option<[u8; 32]>,
        to: Account,
        amount: Nat,
        dedup: Dedup,
    ) -> Result<u64, CallError> {
        ic::icrc1_transfer(ledger, from_subaccount, to, amount, dedup).await
    }

    async fn withdraw_erc20(
        &self,
        minter: Principal,
        ledger: Principal,
        amount: Nat,
        recipient: String,
    ) -> Result<ic::RetrieveErc20Request, CallError> {
        ic::withdraw_erc20(minter, ledger, amount, recipient).await
    }

    async fn withdrawal_status(
        &self,
        minter: Principal,
        id: u64,
    ) -> Result<Vec<ic::WithdrawalDetail>, CallError> {
        ic::withdrawal_status(minter, id).await
    }
}

/// Who and when a step acts as: the clock it records at, the canister's principal and its
/// own EVM address.
#[derive(Clone, Copy, Debug)]
pub struct Ctx {
    pub now: Timestamp,
    pub principal: Principal,
    pub canister: EvmAddress,
}

/// What a move does next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Next {
    Wait,
    /// Decide its route: the cheapest proposal taken, or the canister's own.
    Route,
    /// Sign its pinned order; after a revert, once Swapic says it did not run.
    Sign {
        again: bool,
    },
    /// ICP to Base: ask the ck minter.
    Withdraw,
    /// ICP to Base: read the minter's withdrawal and its landing on Ethereum.
    Landing,
    /// Base to ICP by Eco: find the fill on Ethereum.
    Fill,
    /// Base to ICP by Eco: deposit the landed USDC with the ck helper.
    Deposit,
    /// Base to ICP: sweep the rebalance subaccount once the ckUSDC is there.
    Sweep,
    /// Base to ICP: the sweep's answer was lost: the same transfer again.
    Resweep,
    /// ICP to Base by Eco, past its route's deadline with no arrival: read the destination
    /// Portal (never filled?) and the source's reward (refunded to the canister?) (P11e).
    Unfilled,
    Stuck(String),
}

/// The fill deadline of the Eco intent `order` (`abi.encode(Order)`) publishes, if it
/// publishes one.
pub fn eco_route_deadline(order: &[u8]) -> Option<u64> {
    let order = swapic::decode_order(order)?;
    order
        .steps
        .iter()
        .find_map(|step| swapic::EcoIntent::of_step(step)?.ok())
        .map(|intent| intent.route_deadline)
}

/// The step `item` takes at `now`, pure. `in_flight`: a transaction of it is being signed
/// or is out; `fill_wait`: how long past an Eco route's deadline its fill is first looked for
/// on the pool chain (`agreed::fill_wait`, the payouts' own rule).
pub fn next(item: &Move, now: Timestamp, in_flight: bool, fill_wait: Duration) -> Next {
    if item.stuck.is_some() || item.status.is_final() || in_flight {
        return Next::Wait;
    }
    let waited = now.saturating_duration_since(item.since) >= STUCK_AFTER;
    let window_over = item.urgent || item.window_ends.is_none_or(|end| now >= end);
    let eco = item.route.is_some_and(|route| route.is_eco());
    match (item.direction, &item.status) {
        (MoveDirection::BaseToIcp, MoveStatus::Open)
        | (MoveDirection::IcpToBase, MoveStatus::Landed) => {
            if window_over {
                Next::Route
            } else {
                Next::Wait
            }
        }
        (MoveDirection::IcpToBase, MoveStatus::Open) => Next::Withdraw,
        (MoveDirection::IcpToBase, MoveStatus::Withdrawing) => Next::Stuck(
            "the ck minter's answer was lost: its withdrawals of the canister's account show \
             whether it took the ckUSDC"
                .to_string(),
        ),
        (MoveDirection::IcpToBase, MoveStatus::Withdrawn) if waited => {
            Next::Stuck("the minter's USDC did not land on Ethereum".to_string())
        }
        (MoveDirection::IcpToBase, MoveStatus::Withdrawn) => Next::Landing,
        (MoveDirection::BaseToIcp, MoveStatus::Landed) => Next::Deposit,
        (_, MoveStatus::Routed) => {
            if item.reverts.unwrap_or(0) >= MAX_REVERTS {
                return Next::Stuck(format!("its order reverted {MAX_REVERTS} times"));
            }
            let deadline = item.order.as_ref().map_or(0, |order| order.deadline_s);
            if now.as_secs().get().saturating_add(SIGN_MARGIN_S) >= deadline {
                return Next::Stuck("its order's deadline passed before it ran".to_string());
            }
            let again = item.tx_hash.is_some();
            // whether a reverted order ran is read only at or after the block it reverted
            // in: a revert that carries none can never be read, so a human decides at once
            if again && item.reverted_block.is_none() {
                return Next::Stuck(
                    "its order reverted in no recorded block: whether it ran cannot be read"
                        .to_string(),
                );
            }
            Next::Sign { again }
        }
        (MoveDirection::BaseToIcp, MoveStatus::Burned) if waited => Next::Stuck(if eco {
            "no Eco fill or ckUSDC arrived".to_string()
        } else {
            "no ckUSDC arrived in the rebalance subaccount".to_string()
        }),
        (MoveDirection::BaseToIcp, MoveStatus::Burned) if eco && item.landed.is_none() => {
            Next::Fill
        }
        (MoveDirection::BaseToIcp, MoveStatus::Burned) => Next::Sweep,
        (MoveDirection::BaseToIcp, MoveStatus::Sweeping) => Next::Resweep,
        // an Eco route past its deadline: filled (the arrival is reported), or unfilled and
        // refunded to the canister's Ethereum address in the end (rescued, or at Eco's week)
        (MoveDirection::IcpToBase, MoveStatus::Burned) if eco => {
            let deadline = item
                .order
                .as_ref()
                .and_then(|order| eco_route_deadline(&order.order));
            match deadline {
                Some(deadline)
                    if now.as_secs().get() > deadline.saturating_add(fill_wait.as_secs()) =>
                {
                    Next::Unfilled
                }
                Some(_) => Next::Wait,
                None if waited => Next::Stuck("no arrival on Base".to_string()),
                None => Next::Wait,
            }
        }
        (MoveDirection::IcpToBase, MoveStatus::Burned) if waited => {
            Next::Stuck("no arrival on Base".to_string())
        }
        _ => Next::Wait,
    }
}

/// Whether a transaction of move `id` is being signed or is out.
fn in_flight(id: u64) -> bool {
    crate::tx::in_flight(TxPurpose::PoolMove(id))
}

fn record(step: PoolStep, now: Timestamp) -> Result<(), String> {
    pool_store::record_at(step, now)
        .map(drop)
        .map_err(|error| error.to_string())
}

/// Every open move a step on.
pub async fn drive(now: Timestamp) {
    let moves = read_state(super::open_moves);
    for item in moves {
        let Some(_busy) = Busy::take(move_ref(item.id).into_bytes()) else {
            continue;
        };
        // the move as it is now, after the awaits of the moves before it
        let Some(item) = super::item(item.id) else {
            continue;
        };
        let now = now.max(pool::now());
        let fill_wait = config::with(|config| {
            config.pool_chain.map_or(Duration::ZERO, |chain| {
                crate::agreed::fill_wait_in(config, chain)
            })
        });
        let step = next(&item, now, in_flight(item.id), fill_wait);
        if step == Next::Wait {
            continue;
        }
        if let Err(why) = act(&item, step, now).await {
            pool::log(&format!("rebalance: move {} waits: {why}", item.id));
        }
    }
}

async fn act(item: &Move, step: Next, now: Timestamp) -> Result<(), String> {
    let ctx = Ctx {
        now,
        principal: ic_cdk::api::id(),
        canister: crate::ecdsa::canister_address()
            .await
            .map_err(|error| format!("the canister's address: {error}"))?,
    };
    let outflow = matches!(
        step,
        Next::Route | Next::Sign { .. } | Next::Withdraw | Next::Deposit
    );
    if step == Next::Unfilled && !unfilled_due(item.id, now) {
        return Ok(());
    }
    if outflow {
        if let Some(reason) = pool::pool_closed() {
            return Err(format!("the pool is closed: {reason}"));
        }
    }
    match step {
        Next::Wait => Ok(()),
        Next::Stuck(why) => {
            pool::log(&format!("ALERT: rebalance move {} stopped: {why}", item.id));
            record(PoolStep::MoveStuck { id: item.id, why }, now)
        }
        Next::Route => route(item, now).await,
        Next::Sign { again } => sign(item, again).await,
        Next::Withdraw => withdraw(&Live, item, &ctx).await,
        Next::Landing => landing(&Live, item, &ctx).await,
        Next::Fill => fill(item, &ctx).await,
        Next::Deposit => deposit(item, &ctx).await,
        Next::Sweep => sweep(&Live, item, &ctx).await,
        Next::Resweep => resweep(&Live, item, &ctx).await,
        Next::Unfilled => unfilled(item, &ctx).await,
    }
}

/// The settings, the pool chain and the canister's address, as a step needs them.
struct Setup {
    config: types::Config,
    pool_chain: ChainId,
    settings: RebalanceSettings,
    canister: EvmAddress,
}

async fn setup() -> Result<Setup, String> {
    let config = config::get();
    let pool_chain = config.pool_chain.ok_or("no pool chain is configured")?;
    let settings = read_state(super::settings_in);
    let canister = crate::ecdsa::canister_address()
        .await
        .map_err(|error| format!("the canister's address: {error}"))?;
    Ok(Setup {
        config,
        pool_chain,
        settings,
        canister,
    })
}

/// The chain a move's order runs on.
fn order_chain(item: &Move, pool_chain: ChainId) -> ChainId {
    cost::chains(item.direction, pool_chain).0
}

/// The proposal `item` takes at `now`, pure: the cheapest one taken in the window, never for
/// an urgent move, and only while its order is still more than [`SIGN_MARGIN_S`] from its
/// deadline: one that close would only stop the move as stuck at its first signature
/// (CR-07), so the canister's own route is taken instead.
pub fn proposal_to_take(item: &Move, now: Timestamp) -> Option<&types::pool::Proposal> {
    let proposal = item.proposal.as_ref().filter(|_| !item.urgent)?;
    (proposal.order.deadline_s > now.as_secs().get().saturating_add(SIGN_MARGIN_S))
        .then_some(proposal)
}

/// Decides the route: the proposal to take (`proposal_to_take`), or the canister's own,
/// built here and pinned.
async fn route(item: &Move, now: Timestamp) -> Result<(), String> {
    let setup = setup().await?;
    let (_, dst) = cost::chains(item.direction, setup.pool_chain);
    let (route, order) = match proposal_to_take(item, now) {
        Some(proposal) => (
            MoveRoute::Proposed { fee: proposal.fee },
            proposal.order.clone(),
        ),
        None => own_order(item, &setup, now)?,
    };
    // a Base to ICP move by Eco is filled on Ethereum: its fill is looked for from here
    let scan_from = (item.direction == MoveDirection::BaseToIcp && route.is_eco())
        .then(|| {
            chain_data::fresh(dst, now)
                .map(|reading| BlockNumber::new(reading.block.get().saturating_sub(5)))
        })
        .flatten();
    record(
        PoolStep::MoveRouted {
            id: item.id,
            route,
            order,
            scan_from,
        },
        now,
    )
}

/// The canister's own order for `item`, pinned.
fn own_order(
    item: &Move,
    setup: &Setup,
    now: Timestamp,
) -> Result<(MoveRoute, PinnedOrder), String> {
    let (src, dst) = cost::chains(item.direction, setup.pool_chain);
    let source = setup
        .config
        .evm_chains
        .get(src)
        .ok_or_else(|| format!("chain {src} is not one the canister signs on"))?;
    let destination = setup
        .config
        .evm_chains
        .get(dst)
        .ok_or_else(|| format!("chain {dst} is not one the canister signs on"))?;
    let domain = setup
        .config
        .cctp_domains
        .get(dst)
        .ok_or_else(|| format!("chain {dst} has no CCTP domain"))?;
    let gas_limit = source
        .gas_limit
        .get(GasAction::PoolMove)
        .ok_or_else(|| format!("chain {src} has no pool_move gas row"))?;
    let fast = cost::fast(item.urgent, &setup.settings);
    let max_fee = cost::max_fee(&setup.config, src, item.amount, fast, &setup.settings);
    let order = OwnRoute {
        direction: item.direction,
        source: &source,
        destination: &destination,
        destination_domain: domain.get(),
        canister: setup.canister,
        principal: ic_cdk::api::id(),
    }
    .order(item.id, item.amount, max_fee, fast, now.as_secs().get());
    let pinned = PinnedOrder {
        order_id: swapic::order_id(&order, src, source.swapic).into_bytes(),
        order: swapic::encode_order(&order),
        gas_limit: gas_limit
            .try_into_u128()
            .and_then(|gas| u64::try_from(gas).ok())
            .unwrap_or(u64::MAX),
        deadline_s: u64::try_from(order.deadline).unwrap_or(u64::MAX),
    };
    Ok((MoveRoute::Own { max_fee, fast }, pinned))
}

/// How often an Eco move to Base past its deadline is looked for on the Portals.
pub const UNFILLED_EVERY: Duration = Duration::from_secs(600);

/// Whether move `id`'s Portals are read now: at most every [`UNFILLED_EVERY`], the last read
/// kept in stable memory (C-09); the last reads of moves no longer open are dropped.
fn unfilled_due(id: u64, now: Timestamp) -> bool {
    // the open moves' ids as the ledger holds them: no move is read for them
    let mut open = read_state(|state| state.pool().moves().open);
    open.insert(id);
    crate::storage::timer_state::update(|state| {
        let reads = &mut state.unfilled_reads;
        let kept = reads.len();
        reads.retain(|move_id, _| open.contains(move_id));
        let dropped = reads.len() != kept;
        let due = reads
            .get(&id)
            .is_none_or(|last| now.saturating_duration_since(*last) >= UNFILLED_EVERY);
        if due {
            reads.insert(id, now);
        }
        (due, due || dropped)
    })
}

/// What the Portals say of an Eco move to Base past its deadline, pure: refunded to the
/// canister on Ethereum with no fill on Base, it is routed again (`Ok(true)`); filled, its
/// arrival is the watcher's to report, and after hours it stops for a controller (`Err`);
/// else it waits (`Ok(false)`).
pub fn unfilled_next(fill: types::eco::Fill, reward: u8, waited: bool) -> Result<bool, String> {
    match (fill, reward) {
        (types::eco::Fill::Open | types::eco::Fill::Cancelled, types::eco::REWARD_REFUNDED) => {
            Ok(true)
        }
        (types::eco::Fill::Filled, _) if waited => {
            Err("its Eco intent was filled and no arrival was reported".to_string())
        }
        _ => Ok(false),
    }
}

/// ICP to Base by Eco, past its route's deadline: the destination Portal (Base) never shows
/// it filled and the source Portal (Ethereum) shows its reward refunded to its creator, the
/// canister: the USDC is back at the canister's Ethereum address, and the move is routed
/// again on its own route (P11e). It is never written off while it sits there.
async fn unfilled(item: &Move, ctx: &Ctx) -> Result<(), String> {
    let intent = item
        .intent
        .ok_or("its order published no intent the canister read")?;
    let pool_chain = config::get()
        .pool_chain
        .ok_or("no pool chain is configured")?;
    let deadline = item
        .order
        .as_ref()
        .and_then(|order| eco_route_deadline(&order.order))
        .ok_or("its order's route deadline does not read")?;
    let fill = crate::pool::eco::fill(pool_chain, intent, deadline).await?;
    let reward = crate::pool::eco::reward_status(ChainId::ETHEREUM, intent).await?;
    let waited = ctx.now.saturating_duration_since(item.since) >= STUCK_AFTER;
    match unfilled_next(fill, reward, waited) {
        Ok(true) => {
            pool::log(&format!(
                "rebalance: move {}'s Eco intent was refunded unfilled on Ethereum: routed again",
                item.id
            ));
            record(PoolStep::MoveUnfilled { id: item.id }, ctx.now)
        }
        Ok(false) => Ok(()),
        Err(why) => record(PoolStep::MoveStuck { id: item.id, why }, ctx.now),
    }
}

/// Whether the canister's address on `chain` holds the most `action`'s transaction may cost
/// there (its gas budget): a move is never signed short of gas, and alerts (P11e).
async fn gas_in_hand(chain: ChainId, action: GasAction) -> Result<(), String> {
    let config = config::get();
    let budget = tx::budget_for(&config, chain, action).map_err(|error| error.to_string())?;
    let canister = crate::storage::ecdsa_address::get().ok_or("no canister address yet")?;
    let answers = crate::rpc::rpc_batch(
        chain,
        &[(
            "eth_getBalance",
            json!([crate::rpc::hex0x(canister.as_bytes()), "latest"]),
        )],
        1_024,
    )
    .await
    .map_err(|error| format!("the balance read: {:?}", error.kind()))?;
    let balance = answers
        .first()
        .and_then(Value::as_str)
        .and_then(|text| Wei::from_str_hex(text.strip_prefix("0x")?).ok())
        .ok_or("the balance read answered no amount")?;
    if balance < budget {
        pool::log(&format!(
            "ALERT: the canister holds {balance} wei on chain {chain}, below the {budget} a {action:?} may cost: the move waits"
        ));
        return Err("short of the chain's coin for gas".to_string());
    }
    Ok(())
}

/// Signs `run(order)` of the pinned order on its chain, the canister the payer, against its
/// standing allowance to Swapic there. After a revert only once Swapic says it did not run:
/// a reorg may have turned the revert into a run, and then it never runs again.
async fn sign(item: &Move, again: bool) -> Result<(), String> {
    let pool_chain = config::get()
        .pool_chain
        .ok_or("no pool chain is configured")?;
    let chain = order_chain(item, pool_chain);
    let pinned = item.order.clone().ok_or("the move pinned no order")?;
    gas_in_hand(chain, GasAction::PoolMove).await?;
    if again {
        // `next` holds a revert with no block for a human before it is signed again
        let after = item
            .reverted_block
            .ok_or("its order reverted in no recorded block")?;
        match crate::agreed::ran(chain, pinned.order_id, after).await {
            Ok(false) => {}
            Ok(true) => {
                return record(
                    PoolStep::MoveStuck {
                        id: item.id,
                        why: "its reverted order ran after all".to_string(),
                    },
                    pool::now(),
                )
            }
            Err(why) => return Err(format!("Swapic's record of the order was not read: {why}")),
        }
        if let Some(reason) = pool::pool_closed() {
            return Err(format!("the pool is closed: {reason}"));
        }
    }
    let order = swapic::decode_order(&pinned.order).ok_or("the pinned order does not decode")?;
    let swapic_address = config::get()
        .evm_chains
        .get(chain)
        .map(|evm| evm.swapic)
        .ok_or_else(|| format!("chain {chain} signs nothing"))?;
    tx::create_and_send(
        TxPurpose::PoolMove(item.id),
        chain,
        swapic_address,
        Wei::ZERO,
        swapic::run_calldata(&order),
        GasAmount::from(pinned.gas_limit),
        GasAction::PoolMove,
    )
    .await
    .map(drop)
    .map_err(|error| error.to_string())
}

/// The ck minter's `withdraw_erc20` of the move's ckUSDC to the canister's own Ethereum
/// address: its ckUSDC in flight first, then the approvals of the minter on the ckUSDC and
/// the ckETH ledgers (each with the move's dedup pair, so a retried one is the same), then
/// the call. Refused while the canister's ckETH cannot pay the withdrawal's gas. An answer
/// lost leaves the move withdrawing, and the next tick stops it for a controller.
pub async fn withdraw(calls: &impl IcpCalls, item: &Move, ctx: &Ctx) -> Result<(), String> {
    let flow = flow_settings::get();
    let minter = flow.ck_minter.ok_or("no ck minter is configured")?;
    let cketh = flow.cketh_ledger.ok_or("no ckETH ledger is configured")?;
    let pool_settings = pool_store::ledger()
        .settings
        .ok_or("the pool has no settings")?;
    let ckusdc = pool_settings.ckusdc_ledger;
    let fee = pool_settings
        .fee_of(ckusdc)
        .ok_or("no ckUSDC fee was read")?;
    let gas = Nat::from(flow.cketh_fee_allowance);
    let balance = calls
        .balance(cketh, Account::of(ctx.principal, None))
        .await
        .map_err(|error| format!("the ckETH balance: {error}"))?;
    if balance < gas {
        pool::log(&format!(
            "ALERT: rebalance move {} waits: the canister holds {balance} ckETH, below the {gas} the minter's gas takes",
            item.id
        ));
        return Err("short of ckETH".to_string());
    }
    record(
        PoolStep::MoveWithdrawing {
            id: item.id,
            created_at: ctx.now,
        },
        ctx.now,
    )?;
    let dedup = Dedup {
        memo: move_ref(item.id).into_bytes(),
        created_at_nanos: ctx.now.as_nanos(),
    };
    let refused = |why: String, fee: TokenAmount| {
        record(
            PoolStep::MoveRefused {
                id: item.id,
                why,
                fee,
            },
            ctx.now,
        )
    };
    let amount = item.amount.checked_add(fee).ok_or("the amount overflows")?;
    match calls
        .approve(ckusdc, minter, Nat::from(amount), dedup)
        .await
    {
        Ok(()) => {}
        Err(CallError::Refused(why)) => {
            return refused(
                format!("the ckUSDC approval was refused: {why}"),
                TokenAmount::ZERO,
            )
        }
        Err(error) => return Err(format!("the ckUSDC approval: {error}")),
    }
    match calls.approve(cketh, minter, gas, dedup).await {
        Ok(()) => {}
        Err(CallError::Refused(why)) => {
            return refused(format!("the ckETH approval was refused: {why}"), fee)
        }
        Err(error) => return Err(format!("the ckETH approval: {error}")),
    }
    let recipient = ctx.canister.to_string();
    match calls
        .withdraw_erc20(minter, ckusdc, Nat::from(item.amount), recipient)
        .await
    {
        Ok(request) => record(
            PoolStep::MoveWithdrawn {
                id: item.id,
                withdrawal_id: u64::try_from(request.cketh_block_index.0).unwrap_or(u64::MAX),
                fee,
            },
            ctx.now,
        ),
        Err(CallError::Refused(why)) => refused(why, fee),
        Err(error) => Err(format!("the minter's call: {error}")),
    }
}

/// The landing read at Ethereum's crediting depth: how much of `token` the transaction moved
/// to the canister.
async fn landed_by(
    tx: TxHash,
    canister: EvmAddress,
) -> Result<Option<(TokenAmount, BlockNumber)>, String> {
    let config = config::get();
    let usdc = config
        .evm_chains
        .get(ChainId::ETHEREUM)
        .map(|evm| evm.usdc)
        .ok_or("Ethereum is not a chain the canister signs on")?;
    let depth = crate::tx::credit_confirmations(&config, ChainId::ETHEREUM)
        .map_err(|error| error.to_string())?;
    match crate::evm_reads::transfer_to(ChainId::ETHEREUM, tx, usdc, canister, depth).await {
        Ok(crate::evm_reads::TransferRead::Landed { amount, block }) => Ok(Some((amount, block))),
        Ok(crate::evm_reads::TransferRead::NotYet) => Ok(None),
        Ok(crate::evm_reads::TransferRead::NoTransfer) => {
            Err("the transaction moved no USDC to the canister".to_string())
        }
        Err(error) => Err(format!("the landing read: {:?}", error.kind())),
    }
}

/// The window an ICP to Base move takes proposals in, from its landing: none for an urgent
/// move or a window of nothing.
fn window_from(item: &Move, settings: &RebalanceSettings, now: Timestamp) -> Option<Timestamp> {
    (!item.urgent && !settings.proposal_window.is_zero())
        .then(|| now.checked_add(settings.proposal_window))
        .flatten()
}

/// ICP to Base: the minter's withdrawal, and its USDC at the canister's Ethereum address.
async fn landing(calls: &impl IcpCalls, item: &Move, ctx: &Ctx) -> Result<(), String> {
    let settings = read_state(super::settings_in);
    let minter = flow_settings::get()
        .ck_minter
        .ok_or("no ck minter is configured")?;
    let id = item.withdrawal.ok_or("the move has no withdrawal")?;
    let details = calls
        .withdrawal_status(minter, id)
        .await
        .map_err(|error| format!("the minter's withdrawal: {error}"))?;
    let status = details
        .into_iter()
        .find(|detail| detail.withdrawal_id == id)
        .map(|detail| detail.status)
        .ok_or("the minter knows no such withdrawal")?;
    let hash = match status {
        ic::WithdrawalStatus::TxFinalized(ic::TxFinalizedStatus::Success {
            transaction_hash,
            ..
        }) => transaction_hash,
        // reimbursed: its ckUSDC back at the canister's account, back in the ICP pocket (P11e)
        ic::WithdrawalStatus::TxFinalized(ic::TxFinalizedStatus::Reimbursed {
            reimbursed_amount,
            reimbursed_in_block,
            ..
        }) => {
            let amount = TokenAmount::try_from(reimbursed_amount)
                .map_err(|_| "the reimbursement does not fit")?;
            pool::log(&format!(
                "rebalance: move {}'s withdrawal was reimbursed: {amount} back in the ICP pocket",
                item.id
            ));
            return record(
                PoolStep::MoveReimbursed {
                    id: item.id,
                    amount: amount.min(item.amount),
                    block: u64::try_from(reimbursed_in_block.0).unwrap_or(u64::MAX),
                },
                ctx.now,
            );
        }
        // a reimbursement on its way: it is read again
        ic::WithdrawalStatus::TxFinalized(_) => return Ok(()),
        _ => return Ok(()),
    };
    let tx = hash32(&hash).ok_or("the minter's transaction hash does not read")?;
    let Some((amount, block)) = landed_by(tx, ctx.canister).await? else {
        return Ok(());
    };
    record(
        PoolStep::MoveLanded {
            id: item.id,
            tx_hash: tx,
            block,
            amount,
            window_ends: window_from(item, &settings, ctx.now),
        },
        ctx.now,
    )
}

fn hash32(text: &str) -> Option<TxHash> {
    let bytes = hex::decode(text.strip_prefix("0x")?).ok()?;
    Some(TxHash::new(bytes.try_into().ok()?))
}

/// The one log of `answer` (an `eth_getLogs` answer), its transaction.
fn first_tx(answer: &Value) -> Option<TxHash> {
    answer
        .as_array()?
        .iter()
        .filter(|log| log.get("removed").and_then(Value::as_bool) != Some(true))
        .find_map(|log| hash32(log.get("transactionHash")?.as_str()?))
}

/// Base to ICP by Eco: the fill of its intent on Ethereum (Eco's Portal's `IntentFulfilled`
/// of the intent its order published), and the USDC it delivered to the canister's
/// Ethereum address at the crediting depth.
async fn fill(item: &Move, ctx: &Ctx) -> Result<(), String> {
    let now = ctx.now;
    let config = config::get();
    let intent = item
        .intent
        .ok_or("its order published no intent the canister read")?;
    let portal = config.eco_portal.ok_or("no Eco Portal is configured")?;
    let head = chain_data::fresh(ChainId::ETHEREUM, now)
        .map(|reading| reading.block.get())
        .ok_or("no fresh reading of Ethereum")?;
    let from = item
        .scan_from
        .map_or(head.saturating_sub(FILL_BLOCKS), |from| from.get());
    let to = head.min(from.saturating_add(FILL_BLOCKS - 1));
    let filter = json!({
        "address": crate::rpc::hex0x(portal.as_bytes()),
        "topics": [
            crate::rpc::hex0x(&swapic::topics::intent_fulfilled()),
            crate::rpc::hex0x(&intent),
        ],
        "fromBlock": format!("0x{from:x}"),
        "toBlock": format!("0x{to:x}"),
    });
    let answers = crate::rpc::rpc_batch(
        ChainId::ETHEREUM,
        &[("eth_getLogs", json!([filter]))],
        64 * 1024,
    )
    .await
    .map_err(|error| format!("the fill read: {:?}", error.kind()))?;
    let Some(tx) = answers.first().and_then(first_tx) else {
        return Ok(());
    };
    let Some((amount, block)) = landed_by(tx, ctx.canister).await? else {
        return Ok(());
    };
    record(
        PoolStep::MoveLanded {
            id: item.id,
            tx_hash: tx,
            block,
            amount,
            window_ends: None,
        },
        now,
    )
}

/// Base to ICP by Eco: the ck helper's `depositErc20` of the landed USDC for the canister's
/// OWN principal and rebalance subaccount (constants, never read from a proposal), against
/// its standing allowance to the helper on Ethereum (`pool_approve_ck_helper`).
async fn deposit(item: &Move, ctx: &Ctx) -> Result<(), String> {
    let config = config::get();
    let helper = flow_settings::get()
        .ck_helper
        .ok_or("no ck helper is configured")?;
    let ethereum = config
        .evm_chains
        .get(ChainId::ETHEREUM)
        .ok_or("Ethereum is not a chain the canister signs on")?;
    let gas = ethereum
        .gas_limit
        .get(GasAction::PoolCkDeposit)
        .ok_or("Ethereum has no pool_ck_deposit gas row")?;
    let landed = item.landed.ok_or("nothing landed")?;
    gas_in_hand(ChainId::ETHEREUM, GasAction::PoolCkDeposit).await?;
    let data = ck_deposit_data(
        ethereum.usdc,
        landed.amount,
        ck_word(&ctx.principal),
        rebalance_subaccount(),
    );
    tx::create_and_send(
        TxPurpose::PoolMove(item.id),
        ChainId::ETHEREUM,
        helper,
        Wei::ZERO,
        data,
        gas,
        GasAction::PoolCkDeposit,
    )
    .await
    .map(drop)
    .map_err(|error| error.to_string())
}

/// What a sweep of a subaccount holding `balance` moves and credits, pure: all of it less the
/// ledger's fee, credited at most what the move may carry (a donation is never the pool's);
/// nothing while less than that has arrived.
pub fn swept(
    balance: TokenAmount,
    fee: TokenAmount,
    creditable: TokenAmount,
) -> Option<(TokenAmount, TokenAmount)> {
    if balance < creditable || balance <= fee {
        return None;
    }
    let amount = balance.checked_sub(fee)?;
    Some((amount, amount.min(creditable)))
}

fn ckusdc() -> Result<(candid::Principal, TokenAmount), String> {
    let settings = pool_store::ledger()
        .settings
        .ok_or("the pool has no settings")?;
    let fee = settings
        .fee_of(settings.ckusdc_ledger)
        .ok_or("no ckUSDC fee was read")?;
    Ok((settings.ckusdc_ledger, fee))
}

/// Base to ICP: once the ck minter minted the move's ckUSDC to the rebalance subaccount, all
/// of it to the canister's main account, the move credited at most its amount less its
/// route's cost.
pub async fn sweep(calls: &impl IcpCalls, item: &Move, ctx: &Ctx) -> Result<(), String> {
    let (ledger, fee) = ckusdc()?;
    let subaccount = Account::of(ctx.principal, Some(rebalance_subaccount()));
    let balance = calls
        .balance(ledger, subaccount)
        .await
        .map_err(|error| format!("the subaccount's balance: {error}"))?;
    let balance = TokenAmount::try_from(balance).map_err(|_| "the balance does not fit")?;
    let Some((amount, credited)) = swept(balance, fee, item.creditable()) else {
        return Ok(());
    };
    record(
        PoolStep::MoveSweeping {
            id: item.id,
            created_at: ctx.now,
            amount,
            credited,
        },
        ctx.now,
    )?;
    let item = super::item(item.id).ok_or("the move is gone")?;
    resweep(calls, &item, ctx).await
}

/// The sweep's transfer, with the dedup pair it was recorded with: a retry whose first answer
/// was lost is the same transfer.
pub async fn resweep(calls: &impl IcpCalls, item: &Move, ctx: &Ctx) -> Result<(), String> {
    let (ledger, _) = ckusdc()?;
    let sweep = item.sweep.ok_or("the move has no sweep")?;
    let dedup = Dedup {
        memo: move_ref(item.id).into_bytes(),
        created_at_nanos: sweep.created_at.as_nanos(),
    };
    match calls
        .transfer(
            ledger,
            Some(rebalance_subaccount()),
            Account::of(ctx.principal, None),
            Nat::from(sweep.amount),
            dedup,
        )
        .await
    {
        Ok(block) => record(
            PoolStep::MoveArrived {
                id: item.id,
                credited: sweep.credited,
                credit: None,
                block: None,
                ledger_block: Some(block),
            },
            ctx.now,
        ),
        Err(CallError::Unknown(why)) => Err(format!("the sweep's answer was lost: {why}")),
        Err(error) => record(
            PoolStep::MoveStuck {
                id: item.id,
                why: format!("the sweep: {error}"),
            },
            ctx.now,
        ),
    }
}
