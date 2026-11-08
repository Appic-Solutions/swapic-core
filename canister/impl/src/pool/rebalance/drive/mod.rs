//! The open moves, a step each, on every rebalance tick (the backstop's): a route decided when
//! the window ends, the pinned order signed (and signed again, the same order, after a
//! revert), the ck minter asked, the landing on Ethereum read, an Eco fill found and its ck
//! deposit made, the rebalance subaccount swept. A move's own transactions close in the
//! outbox (`MoveBurned`, `MoveReverted`); its Base arrival is reported by the watcher
//! (`note_move_arrival`) or found by the canister's own read of Base.
//!
//! Every outflow (a signature, the minter's call) waits while the pool is closed. A move
//! that cannot go on stops for a controller (`MoveStuck`), its lines where they are.
//!
//! A withdrawal has landed when its transaction is mined at the crediting depth, successful,
//! with the USDC at the canister's Ethereum address, every node agreeing on that receipt: the
//! minter's `TxFinalized` can lag the mined transaction by days (measured: 336 of 1,617 over 3
//! hours, 7.8 days at worst). A stopped withdrawal is still read, so a transaction mined past
//! the stop lands, and a reimbursement that comes days late is booked, with no controller.

#[cfg(test)]
mod tests;

use super::{cost, own::OwnRoute};
use crate::evm_reads::TransferRead;
use crate::ic::{self, Account, CallError, Dedup};
use crate::pool::ck::AtMinter;
use crate::pool::{self, payout::Busy};
use crate::storage::events::read_state;
use crate::storage::{config, flow_settings, pool as pool_store};
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

/// How long a move may wait in one position before it stops for a controller: the minter's
/// mined transaction (1.8 minutes at the median, 3 hours at the worst of 885) and CCTP's
/// standard attestation, many times over.
pub const STUCK_AFTER: Duration = Duration::from_secs(3 * 3_600);

/// How many times a move's order may revert before it stops for a controller.
pub const MAX_REVERTS: u32 = 3;

/// The minter's standing approval on the ckUSDC ledger, with no expiry: the minter pulls only
/// on a `withdraw_erc20` the canister itself makes. ckETH is approved per move (its gas).
pub const STANDING_APPROVAL: u128 = u128::MAX;

/// The least time the pinned order's deadline must still be ahead to sign it.
pub const SIGN_MARGIN_S: u64 = 120;

/// The most blocks one read for an Eco fill looks at.
pub const FILL_BLOCKS: u64 = 500;

/// The calls a move's legs make, so the driver runs the same against the ledgers, the minter
/// and Ethereum, and against a fake.
#[allow(async_fn_in_trait)]
pub trait IcpCalls {
    async fn balance(&self, ledger: Principal, account: Account) -> Result<Nat, CallError>;
    /// What `spender` may still pull from `owner`'s main account on `ledger`.
    async fn allowance(
        &self,
        ledger: Principal,
        owner: Principal,
        spender: Principal,
    ) -> Result<Nat, CallError>;
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
    ) -> Result<ic::RetrieveErc20Request, ic::WithdrawFailure>;
    async fn withdrawal_status(
        &self,
        minter: Principal,
        id: u64,
    ) -> Result<Vec<ic::WithdrawalDetail>, CallError>;
    /// What the minter's transaction `tx` moved of USDC to the canister on Ethereum.
    async fn landing(&self, tx: TxHash, canister: EvmAddress) -> Result<TransferRead, String>;
}

/// The real ledgers and minter.
pub struct Live;

impl IcpCalls for Live {
    async fn balance(&self, ledger: Principal, account: Account) -> Result<Nat, CallError> {
        ic::icrc1_balance_of(ledger, account).await
    }

    async fn allowance(
        &self,
        ledger: Principal,
        owner: Principal,
        spender: Principal,
    ) -> Result<Nat, CallError> {
        ic::icrc2_allowance(ledger, owner, spender).await
    }

    async fn approve(
        &self,
        ledger: Principal,
        spender: Principal,
        amount: Nat,
        dedup: Dedup,
    ) -> Result<(), CallError> {
        ic::icrc2_approve_once(ledger, None, spender, amount, dedup).await
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
    ) -> Result<ic::RetrieveErc20Request, ic::WithdrawFailure> {
        ic::withdraw_erc20(minter, ledger, amount, recipient, None).await
    }

    async fn withdrawal_status(
        &self,
        minter: Principal,
        id: u64,
    ) -> Result<Vec<ic::WithdrawalDetail>, CallError> {
        ic::withdrawal_status(minter, id).await
    }

    async fn landing(&self, tx: TxHash, canister: EvmAddress) -> Result<TransferRead, String> {
        landing_read(tx, canister).await
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
    /// Portal (never filled?) and the source's reward (refunded to the canister?).
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
    if item.status.is_final() || in_flight {
        return Next::Wait;
    }
    if item.stuck.is_some() {
        let withdrawn =
            item.direction == MoveDirection::IcpToBase && item.status == MoveStatus::Withdrawn;
        return if withdrawn { Next::Landing } else { Next::Wait };
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
    // a stopped withdrawal waits on the minter, for days at the worst
    let late = step == Next::Unfilled || (step == Next::Landing && item.stuck.is_some());
    if late && !late_read_due(item.id, now) {
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
///, so the canister's own route is taken instead.
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
    // a Base to ICP move by Eco is filled on Ethereum: its fill is looked for from a little
    // before the head read now
    let scan_from = if item.direction == MoveDirection::BaseToIcp && route.is_eco() {
        crate::rpc::head(dst)
            .await
            .ok()
            .map(|head| BlockNumber::new(head.get().saturating_sub(5)))
    } else {
        None
    };
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

/// How often a move past its time is read: an Eco move to Base past its deadline on the
/// Portals, a stopped withdrawal at the minter.
pub const LATE_READ_EVERY: Duration = Duration::from_secs(600);

/// Whether move `id` is read now: at most every [`LATE_READ_EVERY`], the last read kept in
/// stable memory; the last reads of moves no longer open are dropped.
fn late_read_due(id: u64, now: Timestamp) -> bool {
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
            .is_none_or(|last| now.saturating_duration_since(*last) >= LATE_READ_EVERY);
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
/// again on its own route. It is never written off while it sits there.
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
/// there (its gas budget): a move is never signed short of gas, and alerts. Then the chain's
/// gas price read, which no call brings a move.
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
    crate::gas::ensure_fresh(chain)
        .await
        .map_err(|failure| format!("the gas price: {failure:?}"))
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
/// the call. Refused while the canister's ckETH cannot pay the withdrawal's gas. A refusal
/// the minter answers burned no ckUSDC, so the move goes back to wait for the next tick,
/// and a refusal that cannot pass also stops for a controller. With no answer the ckUSDC may
/// be burned: the move stays withdrawing, and the next tick stops it for a controller, never
/// asking again by itself, as a second call could burn twice. The minter takes one call at a
/// time from a caller and traps on a second, so the whole of it holds [`AtMinter`], taken
/// before anything is recorded: a move that missed it has not begun.
pub async fn withdraw(calls: &impl IcpCalls, item: &Move, ctx: &Ctx) -> Result<(), String> {
    let Some(_at_minter) = AtMinter::take() else {
        return Err("another withdrawal is at the ck minter".to_string());
    };
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
    // the minter's standing ckUSDC approval (audit C2, decided 2026-11-03): one large
    // approval, asked again only when it reads below what this move pulls, so a move normally
    // pays no ckUSDC approval fee and the minter's answer always books
    let amount = item.amount.checked_add(fee).ok_or("the amount overflows")?;
    let read = |what: &str, error: CallError| format!("the minter's {what} allowance: {error}");
    let usdc_low = calls
        .allowance(ckusdc, ctx.principal, minter)
        .await
        .map_err(|error| read("ckUSDC", error))?
        < Nat::from(amount);
    // the approval's ckUSDC fee, when one is paid, is booked with the answer from what the
    // free line can carry then: a withdrawal may have emptied it during these awaits. Whatever
    // it cannot carry stays off the books (0.01 ckUSDC at a renewal, within the check's
    // tolerance): the answer that names a burn already made is never refused over it
    let booked = |usdc_low: bool| -> TokenAmount {
        if !usdc_low {
            return TokenAmount::ZERO;
        }
        if fee > pool_store::ledger().pockets.icp.free {
            pool::log(&format!(
                "ALERT: rebalance move {}: the ICP pocket's free line cannot carry the minter approval's fee of {fee}: it stays off the books",
                item.id
            ));
            return TokenAmount::ZERO;
        }
        fee
    };
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
    if usdc_low {
        match calls
            .approve(ckusdc, minter, Nat::from(STANDING_APPROVAL), dedup)
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
    }
    // ckETH at the withdrawal's exact gas, each move: the ck payouts' bounded allowance on
    // the same account is their gas-price cap, which a standing one would lift
    match calls.approve(cketh, minter, gas, dedup).await {
        Ok(()) => {}
        Err(CallError::Refused(why)) => {
            return refused(
                format!("the ckETH approval was refused: {why}"),
                booked(usdc_low),
            )
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
                fee: booked(usdc_low),
            },
            ctx.now,
        ),
        Err(ic::WithdrawFailure::Refused(refusal)) => {
            let why = refusal.to_string();
            refused(why.clone(), booked(usdc_low))?;
            if refusal.may_pass() {
                if matches!(refusal, ic::WithdrawErc20Error::CkEthLedgerError { .. }) {
                    pool::log(&format!("ALERT: rebalance move {} waits: {why}", item.id));
                }
                return Ok(());
            }
            pool::log(&format!("ALERT: rebalance move {} stopped: {why}", item.id));
            record(PoolStep::MoveStuck { id: item.id, why }, ctx.now)
        }
        Err(ic::WithdrawFailure::Unknown(why)) => Err(format!("the minter's call: {why}")),
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
        Ok(
            crate::evm_reads::TransferRead::NoTransfer | crate::evm_reads::TransferRead::Reverted,
        ) => Err("the transaction moved no USDC to the canister".to_string()),
        Err(error) => Err(format!("the landing read: {:?}", error.kind())),
    }
}

/// What the minter's transaction `tx` moved of USDC to the canister. One node's read says when
/// it is worth asking: only a landing moves money on, and a node that says otherwise only
/// delays it. A landing is read again by every node: `TxSent` is no word of the minter's that
/// the transaction ran, so the receipt alone decides, and such a read is agreed
/// (`rpc`'s module doc). A move landed on one node's false receipt could never book its
/// reimbursement, and its order would spend other USDC at this address.
async fn landing_read(tx: TxHash, canister: EvmAddress) -> Result<TransferRead, String> {
    let config = config::get();
    let usdc = config
        .evm_chains
        .get(ChainId::ETHEREUM)
        .map(|evm| evm.usdc)
        .ok_or("Ethereum is not a chain the canister signs on")?;
    let depth = crate::tx::credit_confirmations(&config, ChainId::ETHEREUM)
        .map_err(|error| error.to_string())?;
    let one_node = crate::evm_reads::transfer_to(ChainId::ETHEREUM, tx, usdc, canister, depth)
        .await
        .map_err(|error| format!("the landing read: {:?}", error.kind()))?;
    if !matches!(one_node, TransferRead::Landed { .. }) {
        return Ok(one_node);
    }
    crate::pool::arrival::read_transfer(ChainId::ETHEREUM, tx, usdc, canister, depth)
        .await
        .map_err(|error| format!("the agreed landing read: {error:?}"))
}

/// The window an ICP to Base move takes proposals in, from its landing: none for an urgent
/// move or a window of nothing.
fn window_from(item: &Move, settings: &RebalanceSettings, now: Timestamp) -> Option<Timestamp> {
    (!item.urgent && !settings.proposal_window.is_zero())
        .then(|| now.checked_add(settings.proposal_window))
        .flatten()
}

/// What the minter says of a withdrawal, as a landing acts on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MinterSays {
    /// Nothing to act on: no transaction yet, or a failed one's reimbursement on its way.
    Nothing,
    /// `TxSent`: its latest transaction. The minter can replace it, so it is read anew on every
    /// pass and kept only once a receipt proves it.
    Sent(TxHash),
    /// `TxFinalized`, a success.
    Succeeded(TxHash),
    /// `TxFinalized`, reimbursed: the ckUSDC is back at the canister's account.
    Reimbursed { amount: TokenAmount, block: u64 },
}

impl MinterSays {
    pub(crate) fn of(status: ic::WithdrawalStatus) -> Result<Self, String> {
        let tx = |hash: &str| hash32(hash).ok_or("the minter's transaction hash does not read");
        Ok(match status {
            ic::WithdrawalStatus::TxSent(sent) => Self::Sent(tx(&sent.transaction_hash)?),
            ic::WithdrawalStatus::TxFinalized(ic::TxFinalizedStatus::Success {
                transaction_hash,
                ..
            }) => Self::Succeeded(tx(&transaction_hash)?),
            ic::WithdrawalStatus::TxFinalized(ic::TxFinalizedStatus::Reimbursed {
                reimbursed_amount,
                reimbursed_in_block,
                ..
            }) => Self::Reimbursed {
                amount: TokenAmount::try_from(reimbursed_amount)
                    .map_err(|_| "the reimbursement does not fit")?,
                block: u64::try_from(reimbursed_in_block.0).unwrap_or(u64::MAX),
            },
            _ => Self::Nothing,
        })
    }
}

/// What a landing read comes to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LandingStep {
    /// The USDC is at the canister's Ethereum address.
    Land {
        tx: TxHash,
        amount: TokenAmount,
        block: BlockNumber,
    },
    /// The ckUSDC is back in the ICP pocket.
    Reimburse {
        amount: TokenAmount,
        block: u64,
    },
    Wait,
    /// Past its time with neither: a controller looks, and the minter is still heard.
    Stop(String),
}

/// The step a withdrawal of `amount` takes on what the minter `says` and what its transaction
/// showed on Ethereum (`read`, none where it was not asked), `waited` past [`STUCK_AFTER`],
/// pure. Only a transaction that moved at least `amount` to the canister lands: a revert, a
/// transfer short of it or none at all never does, whichever word of the minter's stands
/// beside it. A failed withdrawal's way out is the minter's reimbursement, however late.
pub fn landing_step(
    says: &MinterSays,
    read: Option<TransferRead>,
    amount: TokenAmount,
    waited: bool,
) -> LandingStep {
    match (says, read) {
        (MinterSays::Reimbursed { amount, block }, _) => LandingStep::Reimburse {
            amount: *amount,
            block: *block,
        },
        (
            MinterSays::Sent(tx) | MinterSays::Succeeded(tx),
            Some(TransferRead::Landed {
                amount: landed,
                block,
            }),
        ) if landed >= amount => LandingStep::Land {
            tx: *tx,
            amount: landed,
            block,
        },
        (_, read) if waited => LandingStep::Stop(match read {
            Some(TransferRead::Reverted) => {
                "the minter's transaction reverted: its reimbursement ends this stop".to_string()
            }
            Some(TransferRead::Landed { amount, .. }) => {
                format!("the minter's transaction moved only {amount} to the canister")
            }
            Some(TransferRead::NoTransfer) => {
                "the minter's transaction moved no USDC to the canister".to_string()
            }
            Some(TransferRead::NotYet) | None => {
                "the minter's USDC did not land on Ethereum".to_string()
            }
        }),
        _ => LandingStep::Wait,
    }
}

/// What the minter says of `item`'s withdrawal, and what its transaction shows on Ethereum. A
/// stopped move reads both too, at most every [`LATE_READ_EVERY`]: a transaction mined past
/// the stop lands then, not days later at the minter's `TxFinalized`.
async fn landing_reads(
    calls: &impl IcpCalls,
    item: &Move,
    ctx: &Ctx,
) -> Result<(MinterSays, Option<TransferRead>), String> {
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
    let says = MinterSays::of(status)?;
    let tx = match says {
        MinterSays::Sent(tx) | MinterSays::Succeeded(tx) => Some(tx),
        MinterSays::Nothing | MinterSays::Reimbursed { .. } => None,
    };
    let read = match tx {
        Some(tx) => Some(calls.landing(tx, ctx.canister).await?),
        None => None,
    };
    Ok((says, read))
}

/// ICP to Base: the minter's withdrawal, and its USDC at the canister's Ethereum address. A
/// read that keeps failing stops the move like any other wait.
async fn landing(calls: &impl IcpCalls, item: &Move, ctx: &Ctx) -> Result<(), String> {
    let settings = read_state(super::settings_in);
    let waited = ctx.now.saturating_duration_since(item.since) >= STUCK_AFTER;
    let step = match landing_reads(calls, item, ctx).await {
        Ok((says, read)) => landing_step(&says, read, item.amount, waited),
        Err(why) if waited => LandingStep::Stop(why),
        Err(why) => return Err(why),
    };
    match step {
        LandingStep::Land { tx, amount, block } => record(
            PoolStep::MoveLanded {
                id: item.id,
                tx_hash: tx,
                block,
                amount,
                window_ends: window_from(item, &settings, ctx.now),
            },
            ctx.now,
        ),
        // its ckUSDC back at the canister's account, back in the ICP pocket
        LandingStep::Reimburse { amount, block } => {
            pool::log(&format!(
                "rebalance: move {}'s withdrawal was reimbursed: {amount} back in the ICP pocket",
                item.id
            ));
            record(
                PoolStep::MoveReimbursed {
                    id: item.id,
                    amount: amount.min(item.amount),
                    block,
                },
                ctx.now,
            )
        }
        LandingStep::Stop(why) if item.stuck.is_none() => {
            pool::log(&format!("ALERT: rebalance move {} stopped: {why}", item.id));
            record(PoolStep::MoveStuck { id: item.id, why }, ctx.now)
        }
        LandingStep::Stop(_) | LandingStep::Wait => Ok(()),
    }
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
    let head = crate::rpc::head(ChainId::ETHEREUM)
        .await
        .map_err(|failure| format!("Ethereum's head: {failure:?}"))?
        .get();
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
