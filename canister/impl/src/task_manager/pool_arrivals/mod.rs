//! The canister's own read of Base while pool registrations wait (spec section 5): in case
//! the watcher stopped, every `arrival_backup` the USDC transfers to the canister's address
//! are read from a stored cursor, at most 500 blocks a call (half as many, and half again,
//! while the answer is too large), and each one's transaction goes through the same proof
//! as `note_pool_arrival`. For CCTP and same-chain deposits only: an Eco proof needs the
//! source transaction, which only the watcher brings.
//!
//! What a pass reads is bounded: a transaction whose every transfer is already credited or
//! dust is never read; at most [`MAX_RECEIPTS_PER_PASS`] receipts, whole blocks at a time
//! (the first block always, so the cursor always moves); the cursor stops before the first
//! block left for the next pass, or the first whose read failed. One pass at a time.

#[cfg(test)]
mod tests;

use crate::guards::require_not_halted;
use crate::pool::arrival::{self, find, needs_source, read_base, record, PoolChain};
use crate::rpc::{self, hex0x, parse_block_number, parse_hash32, RpcError};
use crate::state::Store;
use crate::storage::events::read_state;
use crate::storage::{config, pool as pool_store};
use serde_json::{json, Value};
use std::cell::Cell;
use types::pool::{CreditKey, PoolRail, PoolStep};
use types::{BlockNumber, ChainId, Timestamp, TokenAmount, TxHash};

/// The most blocks one `eth_getLogs` reads.
pub const MAX_LOG_BLOCKS: u64 = 500;

/// How far back a read starts when its cursor is older than this: about 20 minutes of Base
/// blocks, longer than any registration waits (15 minutes and the grace).
pub const MAX_LOOKBACK_BLOCKS: u64 = 600;

/// The most one `eth_getLogs` answer may be.
const MAX_LOGS_BYTES: u64 = 256 * 1024;

/// The most receipts one pass reads, but for its first block, which it always reads whole.
pub const MAX_RECEIPTS_PER_PASS: usize = 20;

/// Below this (one cent of USDC) a transfer is dust: no registration's money, and never
/// worth a receipt's read.
pub const DUST: u64 = 10_000;

thread_local! {
    // Heap by necessity: whether a pass is out right now. Two passes from one cursor would
    // read and record the same transfers twice over.
    static PASSING: Cell<bool> = const { Cell::new(false) };
}

/// Held while one pass runs, released on drop, a trap included.
pub struct Pass;

impl Pass {
    pub fn take() -> Option<Self> {
        // lazily: a guard built for a refusal would end the running pass on its drop
        (!PASSING.with(|passing| passing.replace(true))).then(|| Pass)
    }
}

impl Drop for Pass {
    fn drop(&mut self) {
        PASSING.with(|passing| passing.set(false));
    }
}

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

/// One USDC transfer to the canister, as `eth_getLogs` answers it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Logged {
    pub tx: TxHash,
    pub block: BlockNumber,
    pub log_index: u64,
    pub amount: TokenAmount,
}

/// The transfers in `logs` (an `eth_getLogs` answer) that read, a removed one left out, in
/// block and log order.
pub fn logged(logs: &Value) -> Vec<Logged> {
    let mut logged: Vec<Logged> = logs
        .as_array()
        .into_iter()
        .flatten()
        .filter(|log| log.get("removed").and_then(Value::as_bool) != Some(true))
        .filter_map(|log| {
            let hex = |field: &str| log.get(field).and_then(Value::as_str);
            Some(Logged {
                tx: TxHash::new(parse_hash32(log.get("transactionHash"))?),
                block: parse_block_number(hex("blockNumber")?)?,
                log_index: u64::from_str_radix(hex("logIndex")?.strip_prefix("0x")?, 16).ok()?,
                amount: TokenAmount::from_be_bytes(parse_hash32(log.get("data"))?),
            })
        })
        .collect();
    logged.sort_by_key(|one| (one.block, one.log_index));
    logged
}

/// What one pass reads: each transaction worth a receipt's read once, with its block, and
/// the first block left for the next pass. A transaction is worth reading when one of its
/// transfers is neither credited nor dust. Whole blocks at a time, at most `cap` receipts,
/// but the first block always.
pub fn plan_pass(
    logged: &[Logged],
    chain: ChainId,
    credited: impl Fn(&CreditKey) -> bool,
    cap: usize,
) -> (Vec<(TxHash, BlockNumber)>, Option<BlockNumber>) {
    let worth = |one: &Logged| {
        one.amount >= TokenAmount::from(DUST)
            && !credited(&CreditKey {
                chain,
                tx: one.tx,
                log_index: one.log_index,
            })
    };
    let mut read: Vec<(TxHash, BlockNumber)> = Vec::new();
    let mut at = 0;
    while at < logged.len() {
        let block = logged[at].block;
        let end = logged[at..]
            .iter()
            .position(|one| one.block != block)
            .map_or(logged.len(), |len| at + len);
        let mut txs: Vec<(TxHash, BlockNumber)> = Vec::new();
        for one in logged[at..end].iter().filter(|one| worth(one)) {
            if !txs.iter().any(|(tx, _)| *tx == one.tx) && !read.iter().any(|(tx, _)| *tx == one.tx)
            {
                txs.push((one.tx, block));
            }
        }
        if !read.is_empty() && read.len() + txs.len() > cap {
            return (read, Some(block));
        }
        read.extend(txs);
        at = end;
    }
    (read, None)
}

/// The blocks a read covers once `from..=to` answered too large: the first half of them,
/// none when it is one block already.
pub fn halved(from: BlockNumber, to: BlockNumber) -> Option<BlockNumber> {
    (to > from).then(|| BlockNumber::new(from.get() + (to.get() - from.get()) / 2))
}

/// Where the cursor goes after a pass over `from..=to`: to `to` when nothing is left, else
/// to the block before the first one left (`left`), and nowhere when that is `from` itself.
pub fn cursor_after(
    from: BlockNumber,
    to: BlockNumber,
    left: Option<BlockNumber>,
) -> Option<BlockNumber> {
    match left {
        None => Some(to),
        Some(block) if block > from => Some(BlockNumber::new(block.get() - 1)),
        Some(_) => None,
    }
}

/// One pass: nothing while halted or while no CCTP or same-chain registration waits.
pub async fn run_backup() {
    if require_not_halted().is_err() {
        return;
    }
    // one pass at a time: a slow one still out when the timer fires again is left alone
    let Some(_pass) = Pass::take() else {
        return;
    };
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
    let Some((from, mut to)) = range(pool_store::ledger().scan_cursor, safe) else {
        return Ok(());
    };
    // an answer too large is asked again over half the blocks, down to one
    let logs = loop {
        let filter = json!({
            "address": chain.usdc.to_string(),
            "topics": [hex0x(&types::swapic::topics::transfer()), null, hex0x(&chain.canister.to_word())],
            "fromBlock": format!("0x{:x}", from.get()),
            "toBlock": format!("0x{:x}", to.get()),
        });
        match rpc::rpc_batch(
            chain.chain,
            &[("eth_getLogs", json!([filter]))],
            MAX_LOGS_BYTES,
        )
        .await
        {
            Ok(logs) => break logs,
            Err(RpcError::AnswerTooLarge { .. }) => match halved(from, to) {
                Some(less) => to = less,
                None => return Err(format!("block {from}'s transfers are too large to read")),
            },
            Err(error) => return Err(format!("{:?}", error.kind())),
        }
    };
    let logged = logged(logs.first().unwrap_or(&Value::Null));
    let (read, mut left) = plan_pass(
        &logged,
        chain.chain,
        pool_store::credited,
        MAX_RECEIPTS_PER_PASS,
    );
    for (tx, block) in read {
        let base = match read_base(chain, tx).await {
            Ok(base) => base,
            // read again next pass: the cursor stops before its block
            Err(_) => {
                left = Some(left.map_or(block, |first| first.min(block)));
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
        // a step the fold refuses is noted and never read again for it: only a read that
        // failed holds the cursor back
        record(&found, crate::pool::now());
    }
    match cursor_after(from, to, left) {
        Some(block) => pool_store::record(PoolStep::ScanCursor { block })
            .map(drop)
            .map_err(|error| error.to_string()),
        None => Ok(()),
    }
}
