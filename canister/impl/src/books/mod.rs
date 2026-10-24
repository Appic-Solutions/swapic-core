//! The books' side of the canister: the balance check, and the fees read off the receipts
//! of what the canister signed.

#[cfg(test)]
mod tests;

use crate::ecdsa;
use crate::evm_reads::{self, Simulated};
use crate::guards::require_not_halted;
use crate::state::{State, Store};
use crate::storage::events::{append_event, read_state, AppendError};
use crate::storage::halt::is_halted;
use crate::storage::{chain_data, config};
use crate::tx::credit_confirmations;
use serde_json::Value;
use types::books::{assess, pool_open, BalanceCheck, BookStep, PoolClosed, ReadFailure};
use types::{BlockNumber, ChainId, EventType, EvmAddress, Timestamp, TokenAmount, TxHash};

/// Appends one step of `chain_id`'s books.
pub fn record(chain_id: ChainId, step: BookStep) -> Result<(), AppendError> {
    append_event(EventType::Books { chain_id, step }).map(drop)
}

/// Where the fee step pays from and to on a chain: the chain's USDC, its Runner, and this
/// canister.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeePath {
    pub usdc: EvmAddress,
    pub runner: EvmAddress,
    pub canister: EvmAddress,
    /// Eco's Portal, which publishes the intents a receipt carries.
    pub eco_portal: Option<EvmAddress>,
}

/// The network fee an order paid the canister in a receipt (design C, abi.md 6.1): every
/// USDC `Transfer` from the chain's Runner to the canister that is a fee step, read by the one
/// classifier the pool's arrivals read too (`pool::arrival::runner_transfers`): a transfer a
/// `PaidOut(id, canister, USDC, value)` of the Runner in the same receipt explains is a
/// same-chain order's payout to the canister (a pool deposit), never a fee. Nothing for a
/// receipt with none.
pub fn network_fee_in(receipt: &Value, path: FeePath) -> TokenAmount {
    let logs = crate::pool::arrival::logs_of(receipt);
    crate::pool::arrival::runner_transfers(&logs, path.usdc, path.runner, path.canister)
        .into_iter()
        .filter_map(|transfer| match transfer {
            crate::pool::arrival::RunnerTransfer::Fee { amount, .. } => Some(amount),
            crate::pool::arrival::RunnerTransfer::Payout { .. } => None,
        })
        .try_fold(TokenAmount::ZERO, TokenAmount::checked_add)
        .unwrap_or(TokenAmount::MAX)
}

/// Credits `fee` withheld in `tx_hash`, whose receipt is in `block`, to `chain_id`'s fee
/// income. A line the books refuse leaves the fee as surplus, which halts nothing.
pub fn credit_fee(chain_id: ChainId, tx_hash: TxHash, block: BlockNumber, fee: TokenAmount) {
    if fee == TokenAmount::ZERO {
        return;
    }
    if let Err(error) = record(
        chain_id,
        BookStep::FeeEarned {
            tx_hash,
            amount: fee,
            block: Some(block),
        },
    ) {
        ic_cdk::println!("fee {fee} of {tx_hash} on chain {chain_id} not booked: {error}");
    }
}

/// Whether the pool may move money at `now`, read from the fold: the guard every pool
/// outflow calls first, which fails closed (`types::books::pool_open`).
pub fn pool_status<S: Store>(
    state: &State<S>,
    config: &types::Config,
    halted: bool,
    now: Timestamp,
) -> Result<(), PoolClosed> {
    pool_open(
        halted,
        config.pool_chain,
        &state.meta(),
        config.pool_chain.map(|chain| state.book(&chain)),
        config.books_check_interval.0,
        now,
    )?;
    // the ICP pocket's own latch, cleared with the Base one when a controller reopens; and
    // its own staleness once the pool has settings, as the Base books': a read that fails
    // passes nothing
    let pool = state.pool();
    if pool.icp_shortfall_at.is_some() {
        return Err(PoolClosed::IcpShortfall);
    }
    if pool.settings.is_some() {
        let interval = config.books_check_interval.0;
        let checked_at = pool.icp_checked_at.ok_or(PoolClosed::IcpNeverChecked)?;
        let since = now.saturating_duration_since(checked_at);
        if since > interval.saturating_mul(types::books::STALE_INTERVALS) {
            return Err(PoolClosed::IcpStale { since, interval });
        }
    }
    Ok(())
}

/// The pool's guard, now: refused on a halt, a missing pool chain, a controller's pause, a
/// latched shortfall, or the pool chain's books not read within three check intervals,
/// before the first check included. Every pool outflow calls it first.
pub fn require_pool_open() -> Result<(), PoolClosed> {
    let now = Timestamp::from_nanos(ic_cdk::api::time());
    let config = config::get();
    read_state(|state| pool_status(state, &config, is_halted(), now))
}

/// Records a balance check that could not read the balance: a failure is a line in the
/// log, never a skip, and it refreshes nothing the guard reads. Only its kind is recorded
/// or printed, never an error's text, which can carry the provider's url and its key.
pub fn record_failure(chain_id: ChainId, failure: ReadFailure) {
    ic_cdk::println!("the balance check on chain {chain_id} failed: {failure:?}");
    if let Err(error) = record(chain_id, BookStep::CheckFailed { failure }) {
        ic_cdk::println!("the failed check on chain {chain_id} was not recorded: {error}");
    }
}

/// The balance check, every `books_check_interval`: the pool's ICP pocket
/// (`pool::icp::check_icp_pocket`), then per chain the canister signs on, the
/// canister's USDC read at a confirmed block (the freshest pushed head less the chain's
/// crediting depth) beside what the lines expect. It records what it saw and never books it;
/// only a shortfall beyond the tolerance pauses the pool. A chain whose balance cannot be
/// read is recorded as a failed check, so the pool's guard sees its books go stale.
pub async fn check_balances() {
    if require_not_halted().is_err() {
        return;
    }
    let config = config::get();
    // the ICP pocket's own check, which needs no EVM address
    crate::pool::icp::check_icp_pocket(config.books_tolerance.0).await;
    let chains: Vec<(ChainId, EvmAddress)> = config
        .evm_chains
        .0
        .iter()
        .map(|(chain_id, chain)| (*chain_id, chain.usdc))
        .collect();
    let Ok(mine) = ecdsa::canister_address().await else {
        for (chain_id, _) in chains {
            record_failure(chain_id, ReadFailure::NoCanisterAddress);
        }
        return;
    };
    for (chain_id, usdc) in chains {
        if let Err(failure) = check_balance(&config, chain_id, usdc, mine).await {
            record_failure(chain_id, failure);
        }
    }
}

/// What the lines expect of `chain_id`'s balance read at `block`, and whether `observed`
/// falls short of it beyond `tolerance`: the lines as they stand, less the fees booked from
/// blocks above `block`, which a balance read there cannot hold yet (a fee is booked at the
/// swap depth, the read is at the crediting depth behind the head). On the pool chain the
/// pool's own lines are the
/// pool's part (its Base pocket's free and reserved USDC and its unattributed transfers),
/// less every pool arrival from a block above `block`, the same way.
pub fn expected_at<S: Store>(
    state: &State<S>,
    chain_id: ChainId,
    pool_chain: Option<ChainId>,
    block: BlockNumber,
    observed: TokenAmount,
    tolerance: TokenAmount,
) -> (TokenAmount, bool) {
    let books = state.book(&chain_id);
    let mut lines = books
        .total()
        .checked_sub(books.fees_above(block))
        .unwrap_or(TokenAmount::ZERO);
    if pool_chain == Some(chain_id) {
        let pool = state.pool();
        let held = pool
            .base_held()
            .checked_sub(pool.arrivals_above(chain_id, block))
            .unwrap_or(TokenAmount::ZERO);
        lines = lines.checked_add(held).unwrap_or(TokenAmount::MAX);
    }
    assess(lines, observed, tolerance)
}

/// One chain's balance check, or the kind of failure that read no balance.
async fn check_balance(
    config: &types::Config,
    chain_id: ChainId,
    usdc: EvmAddress,
    mine: EvmAddress,
) -> Result<(), ReadFailure> {
    if !config.rpc_urls.contains_key(&chain_id) {
        return Err(ReadFailure::NoRpcUrl);
    }
    let now = Timestamp::from_nanos(ic_cdk::api::time());
    let head = chain_data::fresh(chain_id, now, config.chain_data_max_age)
        .ok_or(ReadFailure::NoChainData)?;
    let depth = credit_confirmations(config, chain_id).map_err(|_| ReadFailure::NoDepth)?;
    let block = head
        .block
        .get()
        .checked_sub(depth.get())
        .ok_or(ReadFailure::BelowDepth)?;
    let block = BlockNumber::new(block);
    let calldata = types::router::balance_of_calldata(mine);
    let observed = match evm_reads::eth_call_at(chain_id, mine, usdc, &calldata, block).await {
        Ok(Simulated::Returned(bytes)) if bytes.len() == 32 => {
            TokenAmount::from_be_bytes(bytes.try_into().expect("BUG: checked to be 32 bytes"))
        }
        Ok(Simulated::Returned(_)) => return Err(ReadFailure::BadAnswer),
        Ok(Simulated::Reverted(_)) => return Err(ReadFailure::Provider),
        Err(error) => return Err(error.kind()),
    };
    // the lines as they stand now, after the read's await
    let (expected, shortfall) = read_state(|state| {
        expected_at(
            state,
            chain_id,
            config.pool_chain,
            block,
            observed,
            config.books_tolerance.0,
        )
    });
    let step = BookStep::Checked {
        check: BalanceCheck {
            block,
            observed,
            expected,
        },
        shortfall,
    };
    if let Err(error) = record(chain_id, step) {
        // the log refused the line itself: nothing more can be recorded now, and the
        // guard sees these books go stale
        ic_cdk::println!("the balance check on chain {chain_id} was not recorded: {error}");
        return Ok(());
    }
    if shortfall {
        ic_cdk::println!(
            "shortfall on chain {chain_id}: {observed} at block {block}, {expected} expected; the pool is paused"
        );
    }
    Ok(())
}
