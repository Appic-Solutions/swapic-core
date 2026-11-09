//! The chain reads that decide money alone, made by every node of the subnet and agreed on
//!: whether Swapic ran an order (before a payout or a move is undone or signed
//! again), and whether an Eco intent was filled on its destination (before an unfilled one
//! is refunded or reopened). Every node reads the head, rounded down so nodes a moment apart
//! agree on it ([`rpc::HEAD_GRAIN`]); every node then reads at one block the crediting depth
//! below it, never `latest`, so honest nodes see the same; and the subnet takes an answer only
//! when enough of them agree (`rpc::rpc_batch_agreed`). No one node or provider picks the
//! block or the answer: a disagreement is no answer, and the read is asked again.

#[cfg(test)]
mod tests;

use crate::rpc::{self, hex0x, parse_block_number, Keep, RpcError};
use crate::storage::config;
use serde_json::{json, Value};
use types::eco::{self, Fill};
use types::{BlockDepth, BlockNumber, ChainId, EvmAddress, TokenAmount};

/// The most one agreed answer of a call may be.
const MAX_ANSWER_BYTES: u64 = 16_384;

/// The most one agreed answer of a block's header and a call may be: a header comes with
/// every transaction hash of its block (and Ethereum's withdrawals), tens of kilobytes on a
/// busy Base or Ethereum block, which the transform then drops.
pub use crate::rpc::MAX_BLOCK_ANSWER_BYTES;

/// Why `claimants` was not read past the time asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FillRead {
    /// No block the nodes agree on is past the time yet: ask again later.
    TooEarly,
    /// Not read now (no agreement, no answer, no Portal): ask again later.
    Unread(String),
}

/// A block header as the nodes agreed on it: its number and its time, in seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub number: BlockNumber,
    pub timestamp: u64,
}

impl Header {
    /// The header an `eth_getBlockByNumber` answer is, None for no block.
    pub fn parse(value: &Value) -> Option<Self> {
        let hex = |name: &str| value.get(name)?.as_str().and_then(parse_block_number);
        Some(Self {
            number: hex("number")?,
            timestamp: hex("timestamp")?.get(),
        })
    }
}

fn block_tag(block: BlockNumber) -> String {
    format!("0x{:x}", block.get())
}

/// `eth_call` of `data` to `to` at `block`, as an agreed batch writes it: no sender, a view.
fn call(to: EvmAddress, data: &[u8], block: BlockNumber) -> Value {
    json!([{"to": to.to_string(), "data": hex0x(data)}, block_tag(block)])
}

/// The one word a call answered, or why there is none.
fn word(answer: Result<Value, RpcError>) -> Result<[u8; 32], String> {
    let answer = answer.map_err(|error| format!("the call: {:?}", error.kind()))?;
    let bytes = answer
        .as_str()
        .and_then(|text| hex::decode(text.strip_prefix("0x")?).ok())
        .ok_or("the call answered no bytes")?;
    <[u8; 32]>::try_from(bytes).map_err(|_| "the call answered no word".to_string())
}

/// The head the nodes agreed on, rounded down to [`rpc::HEAD_GRAIN`], and the chain's
/// crediting depth (how far below it a block no reorg is expected to take back stands). A
/// chain final at its finalized block (BNB Chain, BSC design 6.2.3) answers that block,
/// rounded down the same way, and no depth: every block below it is final.
async fn agreed_head(chain: ChainId) -> Result<(BlockNumber, BlockDepth), String> {
    let (depth, call) = match crate::tx::credit_confirmations(&config::get(), chain) {
        Ok(depth) => (depth, ("eth_blockNumber", json!([]), Keep::Head)),
        Err(crate::tx::NoDepth::Finalized { .. }) => (
            BlockDepth::new(0),
            (
                "eth_feeHistory",
                json!(["0x1", "finalized", []]),
                Keep::FinalizedHead,
            ),
        ),
        Err(_) => return Err(format!("chain {chain} has no depth configured")),
    };
    let mut answers = rpc::rpc_batch_agreed(chain, &[call], rpc::MAX_BLOCK_NUMBER_BYTES.max(1_024))
        .await
        .map_err(|error| format!("the head: {:?}", error.kind()))?;
    let head = answers
        .pop()
        .and_then(Result::ok)
        .as_ref()
        .and_then(Value::as_str)
        .and_then(parse_block_number)
        .ok_or("the head does not read")?;
    Ok((head, depth))
}

/// The block every node reads at: the crediting depth below the agreed head.
async fn agreed_block(chain: ChainId) -> Result<BlockNumber, String> {
    let (head, depth) = agreed_head(chain).await?;
    Ok(deep_below(head, depth))
}

fn deep_below(head: BlockNumber, depth: BlockDepth) -> BlockNumber {
    BlockNumber::new(head.get().saturating_sub(depth.get().max(1)))
}

/// The block `used` is read at, pure: the crediting depth below the agreed head, and never
/// before `after`, the block the transaction reverted in. A block before it says nothing of
/// what the transaction did, whatever it answers: None until a block that late is agreed on
/// (the head is rounded down, so the deep block can trail the transaction by more than the
/// depth).
pub fn read_block(head: BlockNumber, depth: BlockDepth, after: BlockNumber) -> Option<BlockNumber> {
    let deep = deep_below(head, depth);
    (deep >= after).then_some(deep)
}

/// What `owner` holds of `token` on `chain`, in the token's own units, read by every node at one
/// agreed block at the crediting depth (never `latest`), through a transform keeping only the
/// word: the block and the balance. Err: not read now, ask again.
pub async fn balance_of(
    chain: ChainId,
    token: EvmAddress,
    owner: EvmAddress,
) -> Result<(BlockNumber, TokenAmount), String> {
    // a chain final at its own finalized tag is read at that block, as every agreed read is
    let (head, depth) = agreed_head(chain).await?;
    let block = read_block(head, depth, BlockNumber::new(0))
        .ok_or_else(|| format!("chain {chain}'s head is below the crediting depth"))?;
    let data = types::swapic::balance_of_calldata(owner);
    let calls = [("eth_call", call(token, &data, block), Keep::Whole)];
    let mut answers = rpc::rpc_batch_agreed(chain, &calls, MAX_ANSWER_BYTES)
        .await
        .map_err(|error| format!("the balance: {:?}", error.kind()))?;
    let answer = answers.pop().ok_or("the balance was not answered")?;
    Ok((block, TokenAmount::from_be_bytes(word(answer)?)))
}

/// Whether Swapic's `used(order_id)` answer says the order ran.
pub fn ran_of(word: [u8; 32]) -> Result<bool, String> {
    match word {
        word if word == [0; 32] => Ok(false),
        word if word[..31] == [0; 31] && word[31] == 1 => Ok(true),
        _ => Err("Swapic's used answered no boolean".to_string()),
    }
}

/// Whether Swapic on `chain` ran `order_id`, whose transaction reverted in block `after`:
/// every node reads it at one agreed block at least the crediting depth below the head and
/// at or after `after` (never `latest`), through a transform keeping only the word. Err: not
/// read now (no agreed block that late yet, or no agreement), ask again. Replaces
/// `queue::ran` and `rebalance::drive::ran`.
pub async fn ran(chain: ChainId, order_id: [u8; 32], after: BlockNumber) -> Result<bool, String> {
    let swapic = config::get()
        .evm_chains
        .get(chain)
        .map(|evm| evm.swapic)
        .ok_or_else(|| format!("Swapic is not configured on chain {chain}"))?;
    let (head, depth) = agreed_head(chain).await?;
    let block = read_block(head, depth, after).ok_or_else(|| {
        format!("not yet: no agreed block at the crediting depth is at or past block {after}")
    })?;
    let calls = [(
        "eth_call",
        call(swapic, &types::swapic::used_calldata(order_id), block),
        Keep::Whole,
    )];
    let mut answers = rpc::rpc_batch_agreed(chain, &calls, MAX_ANSWER_BYTES)
        .await
        .map_err(|error| format!("Swapic's used: {:?}", error.kind()))?;
    let answer = answers.pop().ok_or("Swapic's used was not answered")?;
    ran_of(word(answer)?)
}

/// How long after a time a read of `chain` for a block past it waits, its crediting `depth`
/// given: the block every node agrees on is the head rounded down to [`rpc::HEAD_GRAIN`]
/// blocks less the depth, so `(HEAD_GRAIN + depth)` of the chain's blocks at the most, and a
/// minute. The one rule for every read past a deadline: a payout's fill, a move's, a
/// refund's proof.
pub fn fill_wait(chain: ChainId, depth: u64) -> std::time::Duration {
    std::time::Duration::from_secs(
        rpc::HEAD_GRAIN
            .saturating_add(depth)
            .saturating_mul(chain.block_seconds())
            .saturating_add(FILL_MARGIN_S),
    )
}

/// [`fill_wait`] at `chain`'s configured crediting depth (none configured: no depth).
pub fn fill_wait_on(chain: ChainId) -> std::time::Duration {
    fill_wait_in(&config::get(), chain)
}

/// [`fill_wait_on`] under `config`, read once by a caller that asks for many chains. The
/// wait only says when a fill is read; with no depth to count (none configured, or a chain
/// final at its finalized block) it counts none, and the read it times is refused for that
/// chain by name (`agreed_block`), so nothing is decided on it.
pub fn fill_wait_in(config: &types::Config, chain: ChainId) -> std::time::Duration {
    let depth = crate::tx::credit_confirmations(config, chain).map_or(0, |depth| depth.get());
    fill_wait(chain, depth)
}

/// What [`fill_wait`] adds for a fill read in the last block before its deadline to be seen
/// first.
pub const FILL_MARGIN_S: u64 = 60;

/// What an agreed header and `claimants` answer say past `after_s`, pure: the fill and its
/// block once the block is past the time, `TooEarly` before it.
pub fn fill_of(
    header: Result<Value, RpcError>,
    claimant: Result<Value, RpcError>,
    after_s: u64,
) -> Result<(Fill, BlockNumber), FillRead> {
    let header =
        header.map_err(|error| FillRead::Unread(format!("the block: {:?}", error.kind())))?;
    let header = Header::parse(&header)
        .ok_or_else(|| FillRead::Unread("the block is not one the nodes see".to_string()))?;
    if header.timestamp <= after_s {
        return Err(FillRead::TooEarly);
    }
    let claimant = word(claimant).map_err(FillRead::Unread)?;
    Ok((eco::fill_of(claimant), header.number))
}

/// `claimants(intent)` on `chain`'s Portal, read by every node at one agreed block whose
/// timestamp is past `after_s`, with that block. `TooEarly` while no agreed block is.
pub async fn fill_after(
    chain: ChainId,
    intent: [u8; 32],
    after_s: u64,
) -> Result<(Fill, BlockNumber), FillRead> {
    let portal = config::get()
        .eco_portal
        .ok_or_else(|| FillRead::Unread("no Eco Portal is configured".to_string()))?;
    let block = agreed_block(chain).await.map_err(FillRead::Unread)?;
    let calls = [
        (
            "eth_getBlockByNumber",
            json!([block_tag(block), false]),
            Keep::Header,
        ),
        (
            "eth_call",
            call(portal, &eco::claimants_calldata(intent), block),
            Keep::Whole,
        ),
    ];
    let answers = rpc::rpc_batch_agreed(chain, &calls, MAX_BLOCK_ANSWER_BYTES)
        .await
        .map_err(|error| FillRead::Unread(format!("the Portal: {:?}", error.kind())))?;
    let mut answers = answers.into_iter();
    let header = answers.next().unwrap_or(Err(RpcError::Unanswered {
        method: "eth_getBlockByNumber".to_string(),
    }));
    let claimant = answers.next().unwrap_or(Err(RpcError::Unanswered {
        method: "eth_call".to_string(),
    }));
    fill_of(header, claimant, after_s)
}

/// What a destination SpokePool says of an Across deposit's fill at an agreed block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcrossFill {
    Filled,
    /// Unfilled, or a slow fill requested and not executed.
    Open,
    /// The SpokePool's own `getV3RelayHash` of the relay is not the canister's (D8): whether
    /// it was filled is not known.
    HashDiffers,
}

/// What an agreed header, `getV3RelayHash(relay)` and `fillStatuses(local)` say past
/// `after_s`, pure: the fill and the header once the block is past the time, `TooEarly`
/// before it.
pub fn across_fill_of(
    header: Result<Value, RpcError>,
    hash: Result<Value, RpcError>,
    status: Result<Value, RpcError>,
    local: [u8; 32],
    after_s: u64,
) -> Result<(AcrossFill, Header), FillRead> {
    let header =
        header.map_err(|error| FillRead::Unread(format!("the block: {:?}", error.kind())))?;
    let header = Header::parse(&header)
        .ok_or_else(|| FillRead::Unread("the block is not one the nodes see".to_string()))?;
    if header.timestamp <= after_s {
        return Err(FillRead::TooEarly);
    }
    if word(hash).map_err(FillRead::Unread)? != local {
        return Ok((AcrossFill::HashDiffers, header));
    }
    let status = word(status).map_err(FillRead::Unread)?;
    match types::swapic::across::FillStatus::of(status) {
        Some(types::swapic::across::FillStatus::Filled) => Ok((AcrossFill::Filled, header)),
        Some(_) => Ok((AcrossFill::Open, header)),
        None => Err(FillRead::Unread(
            "fillStatuses answered no status".to_string(),
        )),
    }
}

/// An Across deposit's fill on `chain`'s SpokePool `spoke_pool`, read by every node at one
/// agreed block whose time is past `after_s` (its fill deadline: no fill lands after it), with
/// that block's header (BSC design 6.2.7). `TooEarly` while no agreed block is.
pub async fn across_fill_after(
    chain: ChainId,
    spoke_pool: EvmAddress,
    relay: &types::swapic::across::RelayData,
    after_s: u64,
) -> Result<(AcrossFill, Header), FillRead> {
    let local = relay.hash(chain);
    let block = agreed_block(chain).await.map_err(FillRead::Unread)?;
    let calls = [
        (
            "eth_getBlockByNumber",
            json!([block_tag(block), false]),
            Keep::Header,
        ),
        (
            "eth_call",
            call(spoke_pool, &relay.hash_calldata(), block),
            Keep::Whole,
        ),
        (
            "eth_call",
            call(
                spoke_pool,
                &types::swapic::across::fill_statuses_calldata(local),
                block,
            ),
            Keep::Whole,
        ),
    ];
    let answers = rpc::rpc_batch_agreed(chain, &calls, MAX_BLOCK_ANSWER_BYTES)
        .await
        .map_err(|error| FillRead::Unread(format!("the SpokePool: {:?}", error.kind())))?;
    let mut answers = answers.into_iter();
    let mut next = |method: &str| {
        answers.next().unwrap_or(Err(RpcError::Unanswered {
            method: method.to_string(),
        }))
    };
    let header = next("eth_getBlockByNumber");
    let hash = next("eth_call");
    let status = next("eth_call");
    across_fill_of(header, hash, status, local, after_s)
}

/// The most one agreed answer of a fill's log pages may be: empty pages, or the one fill.
const MAX_LOGS_ANSWER_BYTES: u64 = 64 * 1024;

/// Whether `answers` (one `eth_getLogs` answer per page) hold a `FilledRelay` of
/// `spoke_pool` that fills the relay `key`, the canister's own deposit, in a log a reorg did not
/// remove, pure; Err when a page was not answered. A fill of another relay that reuses the
/// deposit's origin and id (anyone can fill a relay of their own making) is not the
/// deposit's, and stops nothing.
pub fn relay_logged(
    answers: Vec<Result<Value, RpcError>>,
    spoke_pool: EvmAddress,
    key: &types::swapic::across::RelayKey,
) -> Result<bool, String> {
    let mut found = false;
    for answer in answers {
        let answer = answer.map_err(|error| format!("a page: {:?}", error.kind()))?;
        if !answer.is_array() {
            return Err("a page answered no list".to_string());
        }
        found |= crate::pool::arrival::logs_of(&json!({ "logs": answer }))
            .iter()
            .filter(|log| log.address == spoke_pool)
            .filter_map(|log| types::swapic::across::read_filled_relay(&log.topics, &log.data))
            .any(|filled| filled.key == *key);
    }
    Ok(found)
}

/// Whether `chain`'s SpokePool `spoke_pool` logged a `FilledRelay` of the canister's deposit
/// `relay` in `pages` (each `[from, to]`), read by every node (attack M5: the second,
/// independent check before an unfilled deposit's undo): the logs of its origin and deposit
/// id, each read whole and held to every field of the relay. Err when it was not read.
pub async fn filled_relay_logged(
    chain: ChainId,
    spoke_pool: EvmAddress,
    relay: &types::swapic::across::RelayData,
    pages: &[(u64, u64)],
) -> Result<bool, String> {
    let topics = [
        hex0x(&types::swapic::across::topics::filled_relay()),
        hex0x(&alloy_primitives::U256::from(relay.origin_chain_id.get()).to_be_bytes::<32>()),
        hex0x(&relay.deposit_id.to_u256().to_be_bytes::<32>()),
    ];
    let calls: Vec<(&str, Value, Keep)> = pages
        .iter()
        .map(|(from, to)| {
            let filter = json!({
                "address": spoke_pool.to_string(),
                "topics": topics,
                "fromBlock": format!("0x{from:x}"),
                "toBlock": format!("0x{to:x}"),
            });
            ("eth_getLogs", json!([filter]), Keep::Whole)
        })
        .collect();
    let answers = rpc::rpc_batch_agreed(chain, &calls, MAX_LOGS_ANSWER_BYTES)
        .await
        .map_err(|error| format!("the fill's logs: {:?}", error.kind()))?;
    relay_logged(answers, spoke_pool, &relay.key())
}
