//! The books' side of the canister: the hourly balance check, and the fees read off the
//! receipts of what the canister signed.

#[cfg(test)]
mod tests;

use crate::ecdsa;
use crate::evm_reads::{self, Simulated};
use crate::guards::require_not_halted;
use crate::pool::arrival::{runner_transfers, Receipt, RunnerTransfer};
use crate::state::{State, Store};
use crate::storage::events::{append_event, read_state, AppendError};
use crate::storage::halt::is_halted;
use crate::storage::{config, timer_state};
use crate::tx::credit_confirmations;
use serde_json::Value;
use types::books::{
    assess, pool_open, worth_recording, BalanceCheck, BookStep, PoolClosed, ReadFailure, UserFeeKey,
};
use types::config::EvmChain;
use types::timer_state::{LastRead, TimerState};
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
    fee_steps(&logs, path.usdc, path.runner, path.canister)
}

/// The fee steps in `logs`: every Runner transfer of `usdc` to `canister` no `PaidOut`
/// explains, by the one classifier the pool's arrivals read too.
fn fee_steps(
    logs: &[crate::pool::arrival::Log],
    usdc: EvmAddress,
    runner: EvmAddress,
    canister: EvmAddress,
) -> TokenAmount {
    runner_transfers(logs, usdc, runner, canister)
        .into_iter()
        .filter_map(|transfer| match transfer {
            RunnerTransfer::Fee { amount, .. } => Some(amount),
            RunnerTransfer::Payout { .. } => None,
        })
        .try_fold(TokenAmount::ZERO, TokenAmount::checked_add)
        .unwrap_or(TokenAmount::MAX)
}

/// The line that books `fee` withheld in `tx_hash`, whose receipt is in `block`. Only the
/// pool chain's balance is checked, so only its fees carry the block a check waits for;
/// another chain's would sit in its unseen list for good.
pub fn fee_earned(
    chain_id: ChainId,
    pool_chain: Option<ChainId>,
    tx_hash: TxHash,
    block: BlockNumber,
    fee: TokenAmount,
) -> BookStep {
    BookStep::FeeEarned {
        tx_hash,
        amount: fee,
        block: (pool_chain == Some(chain_id)).then_some(block),
    }
}

/// Credits `fee` withheld in `tx_hash`, whose receipt is in `block`, to `chain_id`'s fee
/// income. A line the books refuse leaves the fee as surplus, which halts nothing.
pub fn credit_fee(chain_id: ChainId, tx_hash: TxHash, block: BlockNumber, fee: TokenAmount) {
    // booked in micro USD, as every chain's fee income counts: a chain whose USDC has more
    // decimals (BNB Chain's 18) is scaled down, floored; the sweep scales it back up
    let (pool_chain, fee) = config::with(|config| {
        let fee = config
            .evm_chains
            .get(chain_id)
            .map_or(fee, |chain| chain.usdc_to_usd6(fee));
        (config.pool_chain, fee)
    });
    if fee == TokenAmount::ZERO {
        return;
    }
    if let Err(error) = record(
        chain_id,
        fee_earned(chain_id, pool_chain, tx_hash, block, fee),
    ) {
        ic_cdk::println!("fee {fee} of {tx_hash} on chain {chain_id} not booked: {error}");
    }
}

/// What a source transaction paid the canister in fee steps, in the source chain's USDC: every
/// `RunnerTransfer::Fee` of its receipt, by the one classifier the pool's arrivals read, so a
/// pool deposit's own transfer is never a fee.
pub fn user_fee_in(receipt: &Receipt, source: &EvmChain, canister: EvmAddress) -> TokenAmount {
    fee_steps(&receipt.logs, source.usdc, source.runner, canister)
}

/// Whether the canister signed `tx`: a relay or a finish (the index, replacements included,
/// and a relay recorded before the index existed by its flow), or a move's or a payout's own
/// source.
pub fn signed_by_canister<S: Store>(state: &State<S>, tx: &TxHash) -> bool {
    let store = state.store();
    store.flow_by_tx(tx).is_some()
        || store.pool_own_source(tx).is_some()
        || store
            .flows()
            .into_iter()
            .any(|(_, flow)| flow.tx_hash.as_ref() == Some(tx))
}

/// Why a source transaction books no user-paid fee.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoUserFee {
    /// It reverted: nothing left the user.
    Reverted,
    /// Its fees are booked already.
    Booked,
    /// The canister signed it: a relay's fee is booked at its close, and a move or a payout
    /// of its own pays none.
    Ours,
    /// It paid the canister nothing.
    NoFee,
}

/// The line that books what `receipt`, a transaction on `chain_id`, paid the canister, pure.
pub fn user_fee_step<S: Store>(
    state: &State<S>,
    chain_id: ChainId,
    pool_chain: Option<ChainId>,
    receipt: &Receipt,
    source: &EvmChain,
    canister: EvmAddress,
) -> Result<BookStep, NoUserFee> {
    if !receipt.success {
        return Err(NoUserFee::Reverted);
    }
    let store = state.store();
    if signed_by_canister(state, &receipt.tx) {
        return Err(NoUserFee::Ours);
    }
    let key = UserFeeKey {
        chain: chain_id,
        tx: receipt.tx,
    };
    if store.is_user_fee_booked(&key) {
        return Err(NoUserFee::Booked);
    }
    // booked in micro USD, as every chain's fee income counts
    let amount = source.usdc_to_usd6(user_fee_in(receipt, source, canister));
    if amount == TokenAmount::ZERO {
        return Err(NoUserFee::NoFee);
    }
    Ok(BookStep::UserFeeEarned {
        tx_hash: receipt.tx,
        amount,
        // only the pool chain's fees carry the block a check waits for, as `fee_earned`'s
        block: (pool_chain == Some(chain_id)).then_some(receipt.block),
    })
}

/// Books what `receipt` paid the canister, once. It never stands in a finish's way: a
/// transaction already booked, or a line the books refuse, answers nothing and the finish goes
/// on; the fee stays surplus until the books hold it. No await between the check and the line.
pub fn book_user_fee(
    chain_id: ChainId,
    receipt: &Receipt,
    source: &EvmChain,
    canister: EvmAddress,
) {
    let pool_chain = config::with(|config| config.pool_chain);
    let step =
        read_state(|state| user_fee_step(state, chain_id, pool_chain, receipt, source, canister));
    if let Ok(step) = step {
        if let Err(error) = record(chain_id, step) {
            ic_cdk::println!(
                "the user fee of {} on chain {chain_id} not booked: {error}",
                receipt.tx
            );
        }
    }
}

/// Whether the pool may move money at `now`, read from the fold and the pockets' balance
/// reads (`reads`): the guard every pool outflow calls first, which fails closed
/// (`types::books::pool_open`).
pub fn pool_status<S: Store>(
    state: &State<S>,
    pool: &types::pool::PoolLedger,
    reads: &TimerState,
    config: &types::Config,
    halted: bool,
    now: Timestamp,
) -> Result<(), PoolClosed> {
    pool_open(
        halted,
        config.pool_chain,
        &state.meta(),
        |chain| reads.books_reads.get(&chain).and_then(LastRead::read_at),
        config.books_check_interval.0,
        now,
    )?;
    // the ICP pocket's own latch, cleared with the Base one when a controller reopens; and
    // its own staleness once the pool has settings, as the Base books': a read that fails
    // passes nothing
    if pool.icp_shortfall_at.is_some() {
        return Err(PoolClosed::IcpShortfall);
    }
    if pool.settings.is_some() {
        let interval = config.books_check_interval.0;
        let checked_at = reads
            .icp_read
            .read_at()
            .ok_or(PoolClosed::IcpNeverChecked)?;
        let since = now.saturating_duration_since(checked_at);
        if since > interval.saturating_mul(types::books::STALE_INTERVALS) {
            return Err(PoolClosed::IcpStale { since, interval });
        }
    }
    Ok(())
}

/// The pool's guard, now: refused on a halt, a missing pool chain, a controller's pause, a
/// latched shortfall, or the pool chain's balance not read within three check intervals,
/// before the first read included. Every pool outflow calls it first.
pub fn require_pool_open() -> Result<(), PoolClosed> {
    let now = Timestamp::from_nanos(ic_cdk::api::time());
    let reads = timer_state::get();
    config::with(|config| {
        read_state(|state| pool_status(state, &state.pool(), &reads, config, is_halted(), now))
    })
}

/// A balance read of `chain_id` that read no balance: printed, of its kind only (never an
/// error's text, which can carry the provider's url and its key), and kept as the time it
/// failed at. It refreshes nothing the guard reads.
fn read_failed(chain_id: ChainId, failure: ReadFailure, now: Timestamp) {
    ic_cdk::println!("the balance check on chain {chain_id} failed: {failure:?}");
    timer_state::update(|state| {
        state.books_reads.entry(chain_id).or_default().failed_at = Some(now);
        ((), true)
    });
}

/// The hourly balance check: the pool's ICP pocket (`pool::icp::check_icp_pocket`), then
/// the pool chain's USDC read at a confirmed block (its own head less the crediting depth)
/// beside what the lines expect. It never books what it saw; only a shortfall beyond the
/// tolerance pauses the pool. A read that passed is kept beside the timers, and one with
/// news goes to the log (`types::books::worth_recording`).
pub async fn check_pool_books() {
    if require_not_halted().is_err() {
        return;
    }
    let config = config::get();
    // the ICP pocket's own check, which needs no EVM address
    crate::pool::icp::check_icp_pocket(config.books_tolerance.0).await;
    let Some(chain_id) = config.pool_chain else {
        return;
    };
    let Some(usdc) = config.evm_chains.0.get(&chain_id).map(|chain| chain.usdc) else {
        ic_cdk::println!("the pool chain {chain_id} is not in the chain table: nothing to check");
        return;
    };
    let read = match ecdsa::canister_address().await {
        Ok(mine) => check_balance(&config, chain_id, usdc, mine).await,
        Err(_) => Err(ReadFailure::NoCanisterAddress),
    };
    if let Err(failure) = read {
        read_failed(
            chain_id,
            failure,
            Timestamp::from_nanos(ic_cdk::api::time()),
        );
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

/// The pool chain's balance check, or the kind of failure that read no balance.
async fn check_balance(
    config: &types::Config,
    chain_id: ChainId,
    usdc: EvmAddress,
    mine: EvmAddress,
) -> Result<(), ReadFailure> {
    if !config.rpc_urls.contains_key(&chain_id) {
        return Err(ReadFailure::NoRpcUrl);
    }
    let depth = credit_confirmations(config, chain_id).map_err(|_| ReadFailure::NoDepth)?;
    let head = crate::rpc::head(chain_id).await?;
    let block = head
        .get()
        .checked_sub(depth.get())
        .ok_or(ReadFailure::BelowDepth)?;
    let block = BlockNumber::new(block);
    let calldata = types::swapic::balance_of_calldata(mine);
    let observed = match evm_reads::eth_call_at(chain_id, mine, usdc, &calldata, block).await {
        Ok(Simulated::Reverted(_)) => return Err(ReadFailure::Provider),
        Ok(answer) => TokenAmount::from_be_bytes(answer.word().ok_or(ReadFailure::BadAnswer)?),
        Err(error) => return Err(error.kind()),
    };
    // the books keep micro USD: a USDC of other decimals is read in them (a no-op on 6)
    let observed = match config.evm_chains.get(chain_id) {
        Some(chain) => chain.usdc_to_usd6(observed),
        None => observed,
    };
    keep_read(
        config,
        chain_id,
        block,
        observed,
        Timestamp::from_nanos(ic_cdk::api::time()),
    )
}

/// Keeps one balance read of `chain_id`, `observed` at `block`, made by `now`: refused
/// below the last read's block; judged against the lines as they stand; in the log when it
/// has news, a shortfall latching the pool's pause; otherwise only the unseen lists drop
/// what it holds. Then it is the chain's last read, unless the log refused its line.
pub fn keep_read(
    config: &types::Config,
    chain_id: ChainId,
    block: BlockNumber,
    observed: TokenAmount,
    now: Timestamp,
) -> Result<(), ReadFailure> {
    // the unseen lists hold nothing at or below the last read any more, so a read further
    // back would expect money its balance cannot hold yet
    if timer_state::get()
        .books_reads
        .get(&chain_id)
        .and_then(LastRead::floor)
        .is_some_and(|floor| block < floor)
    {
        return Err(ReadFailure::Behind);
    }
    let (check, shortfall, recorded) = read_state(|state| {
        let (expected, shortfall) = expected_at(
            state,
            chain_id,
            config.pool_chain,
            block,
            observed,
            config.books_tolerance.0,
        );
        let check = BalanceCheck {
            block,
            observed,
            expected,
        };
        let recorded = state.book(&chain_id).last_check.map(|last| last.surplus());
        (check, shortfall, recorded)
    });
    if worth_recording(recorded, check.surplus(), shortfall) {
        let line = EventType::Books {
            chain_id,
            step: BookStep::Checked { check, shortfall },
        };
        if let Err(error) = crate::storage::events::append_event_at(line, now) {
            // the log refused the line: the read is not kept either, so the guard sees the
            // pool chain's reads go stale
            ic_cdk::println!("the balance check on chain {chain_id} was not recorded: {error}");
            return Ok(());
        }
        if shortfall {
            ic_cdk::println!(
                "shortfall on chain {chain_id}: {observed} at block {block}, {} expected; the pool is paused",
                check.expected
            );
        }
    } else {
        crate::storage::events::forget_seen(chain_id, block);
    }
    timer_state::update(|state| {
        state.books_reads.entry(chain_id).or_default().read = Some((now, check));
        ((), true)
    });
    Ok(())
}
