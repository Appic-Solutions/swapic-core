//! The books' side of the canister: what the flows hold on a chain and have out, the
//! balance check, and the fees read off the receipts of what the canister signed.

#[cfg(test)]
mod tests;

use crate::ecdsa;
use crate::evm_reads::{self, Simulated};
use crate::guards::require_not_halted;
use crate::state::{State, Store};
use crate::storage::events::{append_event, read_state, AppendError};
use crate::storage::halt::is_halted;
use crate::storage::{chain_data, config};
use crate::tx::confirmations;
use serde_json::Value;
use types::books::{assess, pool_open, BalanceCheck, BookStep, PoolClosed};
use types::{ChainId, EventType, EvmAddress, FlowKind, Timestamp, TokenAmount, TxHash};

/// The USDC the canister's flows hold at its address on `chain_id`: what landed for an
/// ICP to EVM quote and has not left yet.
pub fn in_flight<S: Store>(state: &State<S>, chain_id: ChainId) -> TokenAmount {
    state
        .store()
        .flows()
        .into_iter()
        .filter(|(_, flow)| flow.kind == FlowKind::IcpToEvm && flow.chain_id == Some(chain_id))
        .try_fold(TokenAmount::ZERO, |sum, (_, flow)| {
            sum.checked_add(flow.eth_held)
        })
        .unwrap_or(TokenAmount::MAX)
}

/// The USDC the canister signed away on `chain_id` and has no receipt at depth for yet.
pub fn pending_outflows<S: Store>(state: &State<S>, chain_id: ChainId) -> TokenAmount {
    state
        .store()
        .flows()
        .into_iter()
        .filter(|(_, flow)| flow.tx_open && flow.chain_id == Some(chain_id))
        .filter_map(|(_, flow)| flow.continuation.map(|(_, amount)| amount))
        .try_fold(TokenAmount::ZERO, TokenAmount::checked_add)
        .unwrap_or(TokenAmount::MAX)
}

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
}

/// The network fee an order paid the canister in a receipt (design C, abi.md 6.1): every
/// USDC `Transfer` from the chain's Runner to the canister, which is what the fee step
/// makes. Nothing for a receipt with none.
pub fn network_fee_in(receipt: &Value, path: FeePath) -> TokenAmount {
    let topic = types::router::transfer_topic();
    let mut total = TokenAmount::ZERO;
    for log in receipt
        .get("logs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let from_usdc = log
            .get("address")
            .and_then(Value::as_str)
            .and_then(crate::cctp::parse_address)
            == Some(path.usdc);
        let topics: Vec<[u8; 32]> = log
            .get("topics")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|topic| crate::rpc::parse_hash32(Some(topic)))
            .collect();
        let runner_to_canister = topics.len() == 3
            && topics[0] == topic
            && topics[1] == path.runner.to_word()
            && topics[2] == path.canister.to_word();
        if !from_usdc || !runner_to_canister {
            continue;
        }
        let Some(value) = log
            .get("data")
            .and_then(|data| crate::rpc::parse_hash32(Some(data)))
        else {
            continue;
        };
        total = total
            .checked_add(TokenAmount::from_be_bytes(value))
            .unwrap_or(TokenAmount::MAX);
    }
    total
}

/// Credits `fee` withheld in `tx_hash` to `chain_id`'s fee income. A line the books refuse
/// leaves the fee as surplus, which halts nothing.
pub fn credit_fee(chain_id: ChainId, tx_hash: TxHash, fee: TokenAmount) {
    if fee == TokenAmount::ZERO {
        return;
    }
    if let Err(error) = record(
        chain_id,
        BookStep::FeeEarned {
            tx_hash,
            amount: fee,
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
    )
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
/// log, never a skip, and it refreshes nothing the guard reads.
fn record_failure(chain_id: ChainId, reason: String) {
    ic_cdk::println!("the balance check on chain {chain_id} failed: {reason}");
    if let Err(error) = record(chain_id, BookStep::CheckFailed { reason }) {
        ic_cdk::println!("the failed check on chain {chain_id} was not recorded: {error}");
    }
}

/// The balance check, every `books_check_interval`: per chain the canister signs on, the
/// canister's USDC read at a confirmed block (the freshest pushed head less the chain's
/// confirmations) beside what the lines expect. It records what it saw and never books it;
/// only a shortfall beyond the tolerance pauses the pool. A chain whose balance cannot be
/// read is recorded as a failed check, so the pool's guard sees its books go stale.
pub async fn check_balances() {
    if require_not_halted().is_err() {
        return;
    }
    let config = config::get();
    let chains: Vec<(ChainId, EvmAddress)> = config
        .evm_chains
        .0
        .iter()
        .map(|(chain_id, chain)| (*chain_id, chain.usdc))
        .collect();
    let mine = match ecdsa::canister_address().await {
        Ok(mine) => mine,
        Err(error) => {
            for (chain_id, _) in chains {
                record_failure(chain_id, format!("no canister address: {error}"));
            }
            return;
        }
    };
    for (chain_id, usdc) in chains {
        if let Err(reason) = check_balance(&config, chain_id, usdc, mine).await {
            record_failure(chain_id, reason);
        }
    }
}

/// One chain's balance check, or why it read no balance.
async fn check_balance(
    config: &types::Config,
    chain_id: ChainId,
    usdc: EvmAddress,
    mine: EvmAddress,
) -> Result<(), String> {
    if !config.rpc_urls.contains_key(&chain_id) {
        return Err("no rpc url is configured".into());
    }
    let now = Timestamp::from_nanos(ic_cdk::api::time());
    let head =
        chain_data::fresh(chain_id, now, config.chain_data_max_age).ok_or("no fresh chain data")?;
    let depth = confirmations(config, chain_id).map_err(|e| e.to_string())?;
    let block = head
        .block
        .get()
        .checked_sub(depth.get())
        .ok_or("the head is below the confirmation depth")?;
    let block = types::BlockNumber::new(block);
    let calldata = types::router::balance_of_calldata(mine);
    let observed = match evm_reads::eth_call_at(chain_id, mine, usdc, &calldata, block).await {
        Ok(Simulated::Returned(bytes)) if bytes.len() == 32 => {
            TokenAmount::from_be_bytes(bytes.try_into().expect("BUG: checked to be 32 bytes"))
        }
        Ok(Simulated::Returned(bytes)) => {
            return Err(format!("balanceOf answered {} bytes", bytes.len()))
        }
        Ok(Simulated::Reverted(message)) => return Err(format!("balanceOf reverted: {message}")),
        Err(error) => return Err(format!("the read failed: {error}")),
    };
    // the lines as they stand now, after the read's await
    let (expected, shortfall) = read_state(|state| {
        let lines = state.book(&chain_id).total(in_flight(state, chain_id));
        assess(
            lines,
            pending_outflows(state, chain_id),
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
    record(chain_id, step).map_err(|error| format!("the check was not recorded: {error}"))?;
    if shortfall {
        ic_cdk::println!(
            "shortfall on chain {chain_id}: {observed} at block {block}, {expected} expected; the pool is paused"
        );
    }
    Ok(())
}
