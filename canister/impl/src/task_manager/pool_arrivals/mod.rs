//! The canister's own read of Base while pool registrations wait (spec section 5): in case
//! the watcher stopped, every `arrival_backup` the USDC transfers to the canister's address
//! are read from a stored cursor, at most 500 blocks a call (half as many, and half again,
//! while the answer is too large), and each one's transaction goes through the same proof
//! as `note_pool_arrival`. For CCTP and same-chain deposits only: an Eco proof needs the
//! source transaction, which only the watcher brings.
//!
//! What a pass spends is bounded whatever is sent to the canister's address: one head read,
//! at most ten reads of the transfers (500 blocks halved down to one), and at most
//! [`MAX_RECEIPTS_PER_PASS`] receipts out of at most [`MAX_TRANSFERS_PER_PASS`] transfers
//! considered. A transaction whose every transfer is already credited or dust is never read.
//! The cursor always moves, never stalls: it stops before the first block left for the next
//! pass, or the first whose receipt read failed for now; a block too full to read whole (its
//! transfers too large for one answer, or more transactions worth reading than one pass
//! reads), and a receipt that can never be read, are passed over with an alert line, for
//! the watcher (which reports each arrival itself) and ops. One pass at a time.

#[cfg(test)]
mod tests;

use crate::guards::require_not_halted;
use crate::pool::arrival::{self, find, needs_source, read_base, record, PoolChain, Receipt};
use crate::rpc::{self, hex0x, parse_block_number, parse_hash32, RpcError};
use crate::state::Store;
use crate::storage::events::read_state;
use crate::storage::{config, pool as pool_store};
use serde_json::{json, Value};
use settlement_api::types::pool::PoolError;
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

/// The most receipts one pass reads.
pub const MAX_RECEIPTS_PER_PASS: usize = 20;

/// The most transfers one pass considers.
pub const MAX_TRANSFERS_PER_PASS: usize = 1_000;

/// Below this (one cent of USDC) a transfer is dust: no registration's money, and never
/// worth a receipt's read.
pub const DUST: u64 = 10_000;

thread_local! {
    // Heap by necessity: whether a pass is out right now. Two passes from one cursor would
    // read and record the same transfers twice over.
    static PASSING: Cell<bool> = const { Cell::new(false) };
}

/// Held while one pass runs, released on drop: at the pass's every return, and in the
/// cleanup of a trap after one of its awaits.
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

/// What one pass reads of the transfers it considered.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PassPlan {
    /// Each transaction worth a receipt's read, once, with its block.
    pub read: Vec<(TxHash, BlockNumber)>,
    /// The first block left whole for the next pass.
    pub left: Option<BlockNumber>,
    /// A block too full for one pass, passed over: its number, and how many of its
    /// transactions worth reading (or transfers not even considered) were not read.
    pub cut: Option<(BlockNumber, usize)>,
}

/// What one pass reads of `logged`: the first `max_transfers` transfers considered, each
/// transaction worth a receipt's read once (one of its transfers neither credited nor dust),
/// whole blocks at a time, at most `max_receipts`. A first block with more than that is read
/// up to the cap and the rest of it passed over (`cut`), so the cursor always moves; a later
/// one is left whole for the next pass.
pub fn plan_pass(
    logged: &[Logged],
    chain: ChainId,
    credited: impl Fn(&CreditKey) -> bool,
    max_receipts: usize,
    max_transfers: usize,
) -> PassPlan {
    let worth = |one: &Logged| {
        one.amount >= TokenAmount::from(DUST)
            && !credited(&CreditKey {
                chain,
                tx: one.tx,
                log_index: one.log_index,
            })
    };
    let considered = &logged[..logged.len().min(max_transfers)];
    // the block of the first transfer not considered, if any: it is not whole here
    let beyond = logged.get(max_transfers).map(|one| one.block);
    // the first block after `block` with a transfer, considered or not
    let after = |block: BlockNumber| {
        logged
            .iter()
            .find(|one| one.block > block)
            .map(|one| one.block)
    };
    let mut plan = PassPlan::default();
    let mut at = 0;
    while at < considered.len() {
        let block = considered[at].block;
        let end = considered[at..]
            .iter()
            .position(|one| one.block != block)
            .map_or(considered.len(), |len| at + len);
        let mut txs: Vec<(TxHash, BlockNumber)> = Vec::new();
        for one in considered[at..end].iter().filter(|one| worth(one)) {
            if !txs.iter().any(|(tx, _)| *tx == one.tx) {
                txs.push((one.tx, block));
            }
        }
        let whole = beyond != Some(block);
        if whole && plan.read.len() + txs.len() <= max_receipts {
            plan.read.extend(txs);
            at = end;
            continue;
        }
        if !plan.read.is_empty() {
            plan.left = Some(block);
            return plan;
        }
        // the first block, too full for one pass: read up to the cap, pass over the rest
        let unconsidered = logged[at..]
            .iter()
            .filter(|one| one.block == block)
            .count()
            .saturating_sub(end - at);
        let passed_over = txs.len().saturating_sub(max_receipts) + unconsidered;
        txs.truncate(max_receipts);
        plan.read = txs;
        plan.cut = (passed_over > 0).then_some((block, passed_over));
        plan.left = after(block);
        return plan;
    }
    plan.left = beyond;
    plan
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

/// What a pass reads of Base, so it runs the same against the chain and a fake.
#[allow(async_fn_in_trait)]
pub trait BaseReads {
    /// The newest block a pass reads: the head less the crediting depth.
    async fn safe_block(&self) -> Result<BlockNumber, String>;
    /// `eth_getLogs` of the USDC transfers to the canister over `from..=to`.
    async fn transfers(&self, from: BlockNumber, to: BlockNumber) -> Result<Value, RpcError>;
    /// The receipt of `tx`, at the crediting depth.
    async fn receipt(&self, tx: TxHash) -> Result<Receipt, PoolError>;
}

/// Base itself, through `crate::rpc`.
pub struct LiveBase<'a>(pub &'a PoolChain);

impl BaseReads for LiveBase<'_> {
    async fn safe_block(&self) -> Result<BlockNumber, String> {
        let chain = self.0;
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
        Ok(BlockNumber::new(head.get().saturating_sub(depth - 1)))
    }

    async fn transfers(&self, from: BlockNumber, to: BlockNumber) -> Result<Value, RpcError> {
        let chain = self.0;
        let filter = json!({
            "address": chain.usdc.to_string(),
            "topics": [hex0x(&types::swapic::topics::transfer()), null, hex0x(&chain.canister.to_word())],
            "fromBlock": format!("0x{:x}", from.get()),
            "toBlock": format!("0x{:x}", to.get()),
        });
        rpc::rpc_batch(
            chain.chain,
            &[("eth_getLogs", json!([filter]))],
            MAX_LOGS_BYTES,
        )
        .await
        .map(|mut answers| answers.pop().unwrap_or(Value::Null))
    }

    async fn receipt(&self, tx: TxHash) -> Result<Receipt, PoolError> {
        read_base(self.0, tx).await
    }
}

/// One pass: nothing while another is out, while halted, or while no CCTP or same-chain
/// registration waits.
pub async fn run_backup() {
    // first, before anything: a tick while a pass is out leaves it alone
    let Some(_pass) = Pass::take() else {
        return;
    };
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
    if let Err(why) = backup(&LiveBase(&chain), &chain, crate::pool::now).await {
        crate::pool::log(&format!("the pool's own read of Base stopped: {why}"));
    }
}

/// What one pass did: the receipts it read, where it left the cursor, and its alert lines.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PassReport {
    pub receipts: usize,
    pub cursor: Option<BlockNumber>,
    pub alerts: Vec<String>,
}

impl PassReport {
    /// One alert line, logged as it is raised.
    fn alert(&mut self, line: String) {
        crate::pool::log(&line);
        self.alerts.push(line);
    }
}

/// One pass over the blocks after the cursor, read through `reads`.
pub async fn backup(
    reads: &impl BaseReads,
    chain: &PoolChain,
    clock: impl Fn() -> Timestamp,
) -> Result<PassReport, String> {
    let safe = reads.safe_block().await?;
    let mut report = PassReport::default();
    let Some((from, mut to)) = range(pool_store::ledger().scan_cursor, safe) else {
        return Ok(report);
    };
    // an answer too large is asked again over half the blocks; one block that is still too
    // large is passed over, so the cursor never stalls on it
    let logs = loop {
        match reads.transfers(from, to).await {
            Ok(logs) => break logs,
            Err(RpcError::AnswerTooLarge { .. }) => match halved(from, to) {
                Some(less) => to = less,
                None => {
                    report.alert(format!(
                        "ALERT: block {from}'s transfers to the canister are too large to read; \
                         the canister's read of Base passes over it"
                    ));
                    to = from;
                    break Value::Null;
                }
            },
            Err(error) => return Err(format!("{:?}", error.kind())),
        }
    };
    let logged = logged(&logs);
    let plan = plan_pass(
        &logged,
        chain.chain,
        pool_store::credited,
        MAX_RECEIPTS_PER_PASS,
        MAX_TRANSFERS_PER_PASS,
    );
    if let Some((block, passed_over)) = plan.cut {
        report.alert(format!(
            "ALERT: block {block} holds {passed_over} transfers to the canister more than one pass \
             reads; the canister's read of Base passes over them"
        ));
    }
    let mut left = plan.left;
    for (tx, block) in plan.read {
        report.receipts += 1;
        let base = match reads.receipt(tx).await {
            Ok(base) => base,
            // a receipt that can never be read is passed over
            Err(PoolError::BadArrival(why)) => {
                report.alert(format!(
                    "ALERT: the receipt of {tx} cannot be read ({why}); the canister's read of \
                     Base passes over it"
                ));
                continue;
            }
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
            return Ok(report);
        }
        // a step the fold refuses is noted and never read again for it: only a read that
        // failed for now holds the cursor back
        record(&found, clock());
    }
    report.cursor = cursor_after(from, to, left);
    if let Some(block) = report.cursor {
        pool_store::record_at(PoolStep::ScanCursor { block }, clock())
            .map_err(|error| error.to_string())?;
    }
    Ok(report)
}
