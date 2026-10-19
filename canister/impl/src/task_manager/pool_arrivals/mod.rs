//! The canister's own read of Base while pool registrations wait (spec section 5): in case
//! the watcher stopped, every `arrival_backup` the USDC transfers to the canister's address
//! are read from a stored cursor, at most 500 blocks a call, and each one's transaction goes
//! through the same proof as `note_pool_arrival`. For CCTP and same-chain deposits only: an
//! Eco proof needs the source transaction, which only the watcher brings.

#[cfg(test)]
mod tests;

use crate::guards::require_not_halted;
use crate::pool::arrival::{self, find, needs_source, read_base, record, PoolChain};
use crate::rpc::{self, hex0x, parse_block_number, parse_hash32};
use crate::state::Store;
use crate::storage::events::read_state;
use crate::storage::{config, pool as pool_store};
use serde_json::{json, Value};
use types::pool::{PoolRail, PoolStep};
use types::{BlockNumber, Timestamp, TxHash};

/// The most blocks one `eth_getLogs` reads.
pub const MAX_LOG_BLOCKS: u64 = 500;

/// How far back a read starts when its cursor is older than this: about 20 minutes of Base
/// blocks, longer than any registration waits (15 minutes and the grace).
pub const MAX_LOOKBACK_BLOCKS: u64 = 600;

/// The most one `eth_getLogs` answer may be.
const MAX_LOGS_BYTES: u64 = 256 * 1024;

/// Whether a registration the backup can prove waits at `now`.
pub fn has_work<S: Store>(store: &S, now: Timestamp) -> bool {
    pool_store::waiting_in(store, now).iter().any(|r#ref| {
        store.pool_deposit(r#ref).is_some_and(|deposit| {
            matches!(deposit.terms.rail, PoolRail::Cctp | PoolRail::SameChain)
        })
    })
}

/// The blocks the next read covers: after the cursor (or the lookback, when the cursor is
/// older), at most [`MAX_LOG_BLOCKS`], none above `safe`.
pub fn range(cursor: Option<BlockNumber>, safe: BlockNumber) -> Option<(BlockNumber, BlockNumber)> {
    let floor = safe.get().saturating_sub(MAX_LOOKBACK_BLOCKS);
    let from = cursor
        .map_or(floor, |at| at.get().saturating_add(1))
        .max(floor);
    let to = from.saturating_add(MAX_LOG_BLOCKS - 1).min(safe.get());
    (from <= to).then(|| (BlockNumber::new(from), BlockNumber::new(to)))
}

/// The transactions in `logs` (an `eth_getLogs` answer), once each, in log order.
pub fn transactions(logs: &Value) -> Vec<TxHash> {
    let mut seen = Vec::new();
    for log in logs.as_array().into_iter().flatten() {
        if log.get("removed").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        if let Some(tx) = parse_hash32(log.get("transactionHash")).map(TxHash::new) {
            if !seen.contains(&tx) {
                seen.push(tx);
            }
        }
    }
    seen
}

/// One pass: nothing while halted or while no CCTP or same-chain registration waits.
pub async fn run_backup() {
    if require_not_halted().is_err() {
        return;
    }
    let now = crate::pool::now();
    if !read_state(|state| has_work(state.store(), now)) {
        return;
    }
    let Ok(chain) = arrival::pool_chain().await else {
        return;
    };
    if let Err(why) = backup(&chain).await {
        ic_cdk::println!("the pool's own read of Base stopped: {why}");
    }
}

async fn backup(chain: &PoolChain) -> Result<(), String> {
    let head = rpc::rpc_batch(chain.chain, &[("eth_blockNumber", json!([]))], 512)
        .await
        .map_err(|error| format!("{:?}", error.kind()))?
        .first()
        .and_then(Value::as_str)
        .and_then(parse_block_number)
        .ok_or("the head does not read")?;
    let depth = crate::tx::credit_confirmations(&config::get(), chain.chain)
        .map_err(|_| "no depth")?
        .get()
        .max(1);
    let safe = BlockNumber::new(head.get().saturating_sub(depth - 1));
    let Some((from, to)) = range(pool_store::ledger().scan_cursor, safe) else {
        return Ok(());
    };
    let filter = json!({
        "address": chain.usdc.to_string(),
        "topics": [hex0x(&types::swapic::topics::transfer()), null, hex0x(&chain.canister.to_word())],
        "fromBlock": format!("0x{:x}", from.get()),
        "toBlock": format!("0x{:x}", to.get()),
    });
    let logs = rpc::rpc_batch(
        chain.chain,
        &[("eth_getLogs", json!([filter]))],
        MAX_LOGS_BYTES,
    )
    .await
    .map_err(|error| format!("{:?}", error.kind()))?;
    let mut transient = false;
    for tx in transactions(logs.first().unwrap_or(&Value::Null)) {
        let base = match read_base(chain, tx).await {
            Ok(base) => base,
            Err(_) => {
                transient = true;
                continue;
            }
        };
        // an Eco arrival is the watcher's: it alone brings the source transaction
        if needs_source(&base, chain) {
            continue;
        }
        let Ok(found) = find(&base, None, chain) else {
            continue;
        };
        if require_not_halted().is_err() {
            return Ok(());
        }
        if record(&found, crate::pool::now()).is_err() {
            transient = true;
        }
    }
    // a read that failed is read again from the same cursor next time; what was recorded
    // is not recorded twice
    if !transient {
        pool_store::record(PoolStep::ScanCursor { block: to })
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}
