//! The canister's own read of Base (spec section 5), always on once the pool has settings
//! (no registration says when a deposit is on its way): in case the watcher stopped,
//! every `arrival_backup` (an hour) the USDC transfers to the canister's address are read
//! from a stored cursor, at most 500 blocks a pass (half as many, and half again, while the
//! answer is too large), and each one's transaction goes through the same proof as
//! `note_pool_arrival`. A pass that moved the cursor and left it behind the safe block is
//! followed by another a second later, until the read is caught up. It has no terms: a deposit's transfer it proves waits 24 hours for
//! them (anyone may hand them over), then is held for a controller. For CCTP and same-chain
//! deposits only: an Eco proof needs the source transaction, which only the watcher brings.
//!
//! What a pass spends is bounded whatever is sent to the canister's address: one head read,
//! at most ten reads of the transfers (500 blocks halved down to one), and at most
//! [`MAX_RECEIPTS_PER_PASS`] receipts out of at most [`MAX_TRANSFERS_PER_PASS`] transfers
//! considered. A transfer skipped (dust, already credited) is never read and never marked
//! credited.
//!
//! The cursor stands at a block, or at a log inside one, and every transfer at or before it
//! is done: credited (an arrival or an orphan), or read and proven no arrival the canister
//! can credit (no transfer of the pool's, a fee step, an Eco fill whose proof only the
//! watcher brings, a step the fold refused). It never moves past a transfer worth reading
//! that was not read, nor past one whose read failed for now: the next pass starts there,
//! inside the same block if need be, so a block of any size is read over as many passes as
//! it takes. Two things an outsider could send would hold it forever, so they alone are
//! passed over, each with an alert line for ops (the watcher reports each arrival itself):
//! a block whose transfers are too large for one answer even alone, and a receipt too large
//! (or malformed) to ever read. One pass at a time.

#[cfg(test)]
mod tests;

use crate::guards::require_not_halted;
use crate::pool::arrival::{
    self, find, needs_source, read_base, record, FillReads, Judge, PoolChain, Proof, Receipt,
};
use crate::rpc::{self, hex0x, parse_block_number, parse_hash32, RpcError};
use crate::state::Store;
use crate::storage::events::read_state;
use crate::storage::{config, pool as pool_store};
use serde_json::{json, Value};
use settlement_api::types::pool::PoolError;
use std::cell::Cell;
use types::pool::CreditKey;
use types::timer_state::ScanCursor;
use types::{BlockNumber, ChainId, Timestamp, TokenAmount, TxHash};

/// The most blocks one `eth_getLogs` reads.
pub const MAX_LOG_BLOCKS: u64 = 500;

/// How far back the first read starts, before there is a cursor: about 20 minutes of Base
/// blocks. A cursor however far behind is read on from where it stands.
pub const FIRST_LOOKBACK_BLOCKS: u64 = 600;

/// The most one `eth_getLogs` answer may be.
const MAX_LOGS_BYTES: u64 = 256 * 1024;

/// The most receipts one pass reads.
pub const MAX_RECEIPTS_PER_PASS: usize = 20;

/// The most transfers one pass considers.
pub const MAX_TRANSFERS_PER_PASS: usize = 1_000;

/// Below this (ten cents of USDC, the services' own floor: no pool route delivers less) a
/// transfer is dust: no deposit's money, and never worth a receipt's read or an orphan a
/// human must clear.
pub const DUST: u64 = 100_000;

thread_local! {
    // Heap by necessity: whether a pass is out right now. Two passes from one cursor would
    // read and record the same transfers twice over.
    static PASSING: Cell<bool> = const { Cell::new(false) };
    // Heap by necessity too, like PASSING: the failed reads in a row since the last good one;
    // an upgrade that clears it only gives the next failure one more retry.
    static RETRIES: Cell<u32> = const { Cell::new(0) };
}

/// How many times in a row a failed read of Base is tried again soon before it waits for its
/// next interval: about 1 in 50 single node reads fails with a passing connection error (prod,
/// 2026-11-07), and giving up for the whole hour left the read 7 to 12 hours behind.
pub const RETRIES_IN_A_ROW: u32 = 5;

/// How long after a failed read it is tried again.
pub const RETRY_AFTER: std::time::Duration = std::time::Duration::from_secs(30);

/// A new interval, or a read that went through: the next failure may be tried again soon.
pub fn retries_reset() {
    RETRIES.with(|retries| retries.set(0));
}

/// Whether a failed read is tried again soon: at most [`RETRIES_IN_A_ROW`] times in a row.
pub fn retry_soon() -> bool {
    RETRIES.with(|retries| {
        let tried = retries.get();
        let again = tried < RETRIES_IN_A_ROW;
        if again {
            retries.set(tried + 1);
        }
        again
    })
}

/// Whether the pool's own read of Base has work (no registration tells it when a
/// deposit is on its way): once the pool has settings, always.
pub fn has_work<S: Store>(store: &S) -> bool {
    store.pool_ledger().settings.is_some()
}

/// The blocks the next read covers: after the cursor, from its own block when it stands
/// inside one (`log`), or from [`FIRST_LOOKBACK_BLOCKS`] back when there is no cursor yet;
/// at most [`MAX_LOG_BLOCKS`], none above `safe`.
pub fn range(
    cursor: Option<BlockNumber>,
    log: Option<u64>,
    safe: BlockNumber,
) -> Option<(BlockNumber, BlockNumber)> {
    let from = match (cursor, log) {
        (Some(at), Some(_)) => at.get(),
        (Some(at), None) => at.get().saturating_add(1),
        (None, _) => safe.get().saturating_sub(FIRST_LOOKBACK_BLOCKS),
    };
    let to = from.saturating_add(MAX_LOG_BLOCKS - 1).min(safe.get());
    (from <= to).then(|| (BlockNumber::new(from), BlockNumber::new(to)))
}

/// Where a transfer sits: its block and its log.
pub type Position = (BlockNumber, u64);

/// One USDC transfer to the canister, as `eth_getLogs` answers it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Logged {
    pub tx: TxHash,
    pub block: BlockNumber,
    pub log_index: u64,
    pub amount: TokenAmount,
}

impl Logged {
    pub fn position(&self) -> Position {
        (self.block, self.log_index)
    }
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

/// Moves the stored cursor to `to`, never back: a block done whole is past any log of it.
pub fn advance_cursor(to: ScanCursor) {
    crate::storage::timer_state::update(|state| {
        let ahead = state
            .scan_cursor
            .is_none_or(|at| to.position() >= at.position());
        if ahead {
            state.scan_cursor = Some(to);
        }
        ((), ahead)
    });
}

/// How far a pass got.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Done {
    /// Every transfer of the range.
    Range,
    /// Every transfer at or before this one.
    Through(Position),
    /// Nothing new.
    Nothing,
}

/// What one pass reads of the transfers after the cursor: the first `max_transfers`
/// considered, each transaction worth a receipt's read (one of its transfers neither
/// credited nor dust) once, in order, at most `max_receipts`; and how far that gets the read
/// when every receipt is read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PassPlan {
    /// Each transaction to read, with the position of its first transfer.
    pub read: Vec<(TxHash, Position)>,
    pub done: Done,
}

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
    let through = |at: usize| {
        at.checked_sub(1).map_or(Done::Nothing, |before| {
            Done::Through(considered[before].position())
        })
    };
    let mut read: Vec<(TxHash, Position)> = Vec::new();
    for (at, one) in considered.iter().enumerate() {
        if !worth(one) || read.iter().any(|(tx, _)| *tx == one.tx) {
            continue;
        }
        // the cap: done through the transfer before the first left unread
        if read.len() >= max_receipts {
            return PassPlan {
                read,
                done: through(at),
            };
        }
        read.push((one.tx, one.position()));
    }
    let done = if considered.len() < logged.len() {
        through(considered.len())
    } else {
        Done::Range
    };
    PassPlan { read, done }
}

/// How far a pass got once some receipts failed to read for now: through the transfer before
/// the first of them (`failed`, the position of its first transfer), else as planned.
pub fn done_after(logged: &[Logged], planned: Done, failed: Option<Position>) -> Done {
    let Some(failed) = failed else {
        return planned;
    };
    logged
        .iter()
        .rev()
        .find(|one| one.position() < failed)
        .map_or(Done::Nothing, |one| Done::Through(one.position()))
}

/// The blocks a read covers once `from..=to` answered too large: the first half of them,
/// none when it is one block already.
pub fn halved(from: BlockNumber, to: BlockNumber) -> Option<BlockNumber> {
    (to > from).then(|| BlockNumber::new(from.get() + (to.get() - from.get()) / 2))
}

/// What a pass reads of Base (and of an Eco refund's destination Portal), so it runs the
/// same against the chains and a fake.
#[allow(async_fn_in_trait)]
pub trait BaseReads: FillReads {
    /// The newest block a pass reads: the head less the crediting depth.
    async fn safe_block(&self) -> Result<BlockNumber, String>;
    /// `eth_getLogs` of the USDC transfers to the canister over `from..=to`.
    async fn transfers(&self, from: BlockNumber, to: BlockNumber) -> Result<Value, RpcError>;
    /// The receipt of `tx`, at the crediting depth.
    async fn receipt(&self, tx: TxHash) -> Result<Receipt, PoolError>;
    /// The memos the chain shows for the deposits among `found` in `tx`
    /// (`arrival::memos_from_chain`); an error for a read that failed for now.
    async fn memos(
        &self,
        tx: TxHash,
        base: &Receipt,
        found: &[arrival::Found],
    ) -> Result<Vec<Vec<u8>>, PoolError>;
}

/// Base itself, through `crate::rpc`.
pub struct LiveBase<'a>(pub &'a PoolChain);

impl BaseReads for LiveBase<'_> {
    async fn safe_block(&self) -> Result<BlockNumber, String> {
        let chain = self.0;
        let head = rpc::head(chain.chain)
            .await
            .map_err(|failure| format!("the head: {failure:?}"))?;
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

    async fn memos(
        &self,
        tx: TxHash,
        base: &Receipt,
        found: &[arrival::Found],
    ) -> Result<Vec<Vec<u8>>, PoolError> {
        arrival::memos_from_chain(tx, base, found, self.0, None, &arrival::LiveSource).await
    }
}

impl FillReads for LiveBase<'_> {
    async fn fill(
        &self,
        chain: ChainId,
        intent: [u8; 32],
        route_deadline: u64,
    ) -> Result<types::eco::Fill, String> {
        crate::pool::arrival::LivePortal
            .fill(chain, intent, route_deadline)
            .await
    }
}

/// One pass: nothing while another is out, while halted, or while no CCTP or same-chain
/// registration waits.
pub async fn run_backup() {
    // first, before anything: a tick while a pass is out leaves it alone
    let Some(_pass) = crate::guards::Running::take(&PASSING) else {
        return;
    };
    if require_not_halted().is_err() {
        return;
    }
    if !read_state(|state| has_work(state.store())) {
        return;
    }
    let Ok(chain) = arrival::pool_chain().await else {
        return;
    };
    match backup(&LiveBase(&chain), &chain, crate::pool::now).await {
        Ok(report) => {
            retries_reset();
            if report.again {
                crate::task_manager::read_base_again();
            }
        }
        Err(why) if retry_soon() => {
            crate::pool::log(&format!(
                "the pool's own read of Base failed for now, read again in {} s: {why}",
                RETRY_AFTER.as_secs()
            ));
            crate::task_manager::read_base_after(RETRY_AFTER);
        }
        Err(why) => crate::pool::log(&format!(
            "the pool's own read of Base stopped until its next interval: {why}"
        )),
    }
}

/// What one pass did: the receipts it read, where it left the cursor (a block, and the log
/// of it the read is done through when it stands inside it), its alert lines, and whether
/// another pass should follow at once (it moved the cursor and left it behind).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PassReport {
    pub receipts: usize,
    pub cursor: Option<(BlockNumber, Option<u64>)>,
    pub alerts: Vec<String>,
    pub again: bool,
}

impl PassReport {
    /// One alert line, logged as it is raised.
    fn alert(&mut self, line: String) {
        crate::pool::log(&line);
        self.alerts.push(line);
    }
}

/// Whether the read of Base stops before a transaction, for what its transfers' proofs say,
/// and the alert's reason: a transfer not proven yet is read again next pass (an ALERT); an
/// Eco refund read too early waits for its time with no alert.
pub fn holds_back(found: &[arrival::Found]) -> (bool, Option<String>) {
    let unproven = found.iter().find_map(|one| match &one.proof {
        Proof::Unproven { why } => Some(why.clone()),
        _ => None,
    });
    let early = found
        .iter()
        .any(|one| matches!(one.proof, Proof::TooEarly { .. }));
    (unproven.is_some() || early, unproven)
}

/// One pass over the blocks after the cursor, read through `reads`.
pub async fn backup(
    reads: &impl BaseReads,
    chain: &PoolChain,
    clock: impl Fn() -> Timestamp,
) -> Result<PassReport, String> {
    let safe = reads.safe_block().await?;
    let mut report = PassReport::default();
    let stored = crate::storage::timer_state::get().scan_cursor;
    let (cursor, cursor_log) = (stored.map(|at| at.block), stored.and_then(|at| at.log));
    let Some((from, mut to)) = range(cursor, cursor_log, safe) else {
        return Ok(report);
    };
    // an answer too large is asked again over half the blocks; one block that is still too
    // large is passed over, so the cursor never stalls on it
    let mut overfull = false;
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
                    overfull = true;
                    break Value::Null;
                }
            },
            Err(error) => return Err(format!("{:?}", error.kind())),
        }
    };
    let mut logged = logged(&logs);
    // the transfers of the cursor's own block it is done through already
    if let (Some(at), Some(log)) = (cursor, cursor_log) {
        logged.retain(|one| one.block != at || one.log_index > log);
    }
    let plan = plan_pass(
        &logged,
        chain.chain,
        pool_store::credited,
        MAX_RECEIPTS_PER_PASS,
        MAX_TRANSFERS_PER_PASS,
    );
    let mut failed: Option<Position> = None;
    for (tx, first) in plan.read {
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
            // read again next pass: the cursor stops before it
            Err(_) => {
                failed = Some(failed.map_or(first, |earlier| earlier.min(first)));
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
        // an Eco refund only once its intent's destination shows it never filled
        let found = crate::pool::arrival::prove_refunds(found, reads, clock()).await;
        if require_not_halted().is_err() {
            return Ok(report);
        }
        // a refund whose fill was not read is read again next pass, like a receipt that
        // failed: the cursor stops before it, and the rest of it is credited now
        let (held, why) = holds_back(&found);
        if let Some(why) = why {
            report.alert(format!(
                "ALERT: a transfer in {tx} is not proven yet ({why}); the canister's read of \
                 Base stops before it and asks again"
            ));
        }
        if held {
            failed = Some(failed.map_or(first, |earlier| earlier.min(first)));
        }
        // the terms the chain shows (a same-chain order's own `OrderStarted`, a CCTP
        // deposit's source the canister finished itself), else the transfer awaits them; a
        // step the fold refuses is noted, and read: only a read that failed for now holds
        // the cursor back, a memo's as a receipt's, and records nothing of the transaction
        let memos = match reads.memos(tx, &base, &found).await {
            Ok(memos) => memos,
            Err(error) => {
                report.alert(format!(
                    "ALERT: a deposit's memo in {tx} did not read ({error:?}); the canister's \
                     read of Base stops before it and asks again"
                ));
                failed = Some(failed.map_or(first, |earlier| earlier.min(first)));
                continue;
            }
        };
        if require_not_halted().is_err() {
            return Ok(report);
        }
        let givens = arrival::givens_of(&memos, chain.chain);
        let judge = Judge::of(&givens);
        // a deposit while the pool is closed is not now (audit C13): the cursor stops before
        // it, and the same transfer is credited once the pool reopens
        if judge.closed.is_some() && !givens.is_empty() {
            failed = Some(failed.map_or(first, |earlier| earlier.min(first)));
            continue;
        }
        record(&found, &judge, clock());
    }
    let done = if overfull {
        Done::Range
    } else {
        done_after(&logged, plan.done, failed)
    };
    report.cursor = match done {
        Done::Range => Some((to, None)),
        Done::Through((block, log)) => Some((block, Some(log))),
        Done::Nothing => None,
    };
    if let Some((block, log)) = report.cursor {
        advance_cursor(ScanCursor { block, log });
    }
    let after = crate::storage::timer_state::get().scan_cursor;
    let moved = after.map(|at| at.position()) > stored.map(|at| at.position());
    report.again =
        moved && range(after.map(|at| at.block), after.and_then(|at| at.log), safe).is_some();
    Ok(report)
}
