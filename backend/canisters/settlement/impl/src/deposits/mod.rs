//! The deposit read: whether a user's funds for a quote have arrived in a chain's vault,
//! read from the chain and held to the configured depth. It is the read `claim_swap`
//! decides money on, so every answer is bound to what this canister asked for: the vault
//! it configured, the event the vault emits, the quote it is claiming, the token and the
//! amount it is looking for, and the head the same provider answered first.
//!
//! The vault marks a quote per payer, so anyone can log a deposit under a quote's hash
//! ahead of the user's. The deposit that counts is therefore the one that matches what
//! the read wants, among every log the hash names, and not the first one logged. Among
//! several that match, it is the oldest one in the whole range that is deep enough to
//! decide on, wherever the read's windows fall; a match not deep enough yet is reported
//! only when the range holds no deep one.
//!
//! Every read starts by asking the provider for its own head, alone, and builds its
//! windows from it: the anchor is the lower of the watcher's fresh reading and that head,
//! so a head the watcher pushed ahead of the chain cannot put a window past the one the
//! provider serves. Geth-family providers refuse a range above their head outright, and a
//! refused range is a claim that can never be made.

#[cfg(test)]
mod tests;

use crate::rpc::{self, hex0x, parse_block_number, parse_hash32, RpcError, MAX_BLOCK_NUMBER_BYTES};
use crate::storage::{chain_data, config};
use crate::tx::{confirmations, NoDepth};
use serde_json::{json, Value};
use thiserror::Error;
pub use types::abi::deposited_topic;
use types::config::DepositLookback;
use types::evm::EvmAddressError;
use types::tx::is_confirmed;
use types::{
    BlockDepth, BlockNumber, ChainId, EvmAddress, QuoteHash, Timestamp, TokenAmount, TxHash,
    UnixSeconds,
};

/// The most one `eth_getLogs` answer may be: enough for a few dozen deposits naming one
/// quote (forty dust logs ahead of the real one still fit, at about seven hundred bytes
/// each as a provider prints a log), and refunded when unused under pay-as-you-go
/// pricing. An answer over it is refused by the system, which the read reports as a
/// typed transport failure rather than deciding on a part of the log.
const MAX_LOGS_BYTES: u64 = 32 * 1024;

/// The window and the window cap, kept beside the lookback knob they bound (see
/// `types::config`), so the knob's ceiling is what this read can walk.
pub use types::config::{LOGS_WINDOW_BLOCKS, MAX_LOGS_WINDOWS};

/// How far below the height a quote was registered at a claim's read starts, in blocks of
/// the quote's source chain. That height is the watcher's reading, so the claim trusts it
/// only this far:
///
/// - two providers disagree on the head by a few blocks, so an honest height can sit a
///   little above the block the user's deposit landed in;
/// - a watcher can push a head ahead of the chain. The read starts this far below the
///   lower of that height and the provider's own head, so every block the chain made
///   after the registration is inside the read as long as the chain made no more than
///   this many between the registration and the claim, and the oldest matching deposit
///   in it is the user's.
///
/// Twenty thousand blocks, two windows. On Arbitrum, the fastest chain read at four
/// blocks a second, a claim's default life (a 45 second quote, the 120 second permit
/// window and the hour of grace, 3,765 seconds, 15,060 blocks) fits inside it with room,
/// and on every slower chain it fits many times over. A deploy that raises the grace past
/// about eighty minutes leaves a claim asked that late on Arbitrum partly outside it: the
/// rest of the lookback is still read when nothing matches above the floor, so what is
/// left is a later deposit of the same quote above the floor being taken first.
pub const FLOOR_MARGIN_BLOCKS: u64 = 2 * LOGS_WINDOW_BLOCKS;

/// How far back a waiting arrival read looks: one window, the newest, so a tick costs the
/// swap one window. The whole lookback is read only before a refund.
const ONE_WINDOW: DepositLookback = DepositLookback::new(LOGS_WINDOW_BLOCKS as u32);

/// The most one `eth_getBlockByHash` answer may be, asked without the transactions' bodies:
/// the header and the hash of every transaction in the block.
///
/// - The header is under five kilobytes as a provider prints it, with Ethereum's sixteen
///   withdrawals, the 514-character logs bloom and BSC's longest extra data.
/// - Each transaction hash is 69 bytes: 66 characters, two quotes and a comma.
/// - The most transactions a block holds is its gas limit over the 21,000 gas of a plain
///   transfer. At 160 million gas, above the per-block limit of every chain this canister
///   reads, that is 7,619 hashes, 525,711 bytes.
///
/// About 531 kilobytes in all, doubled for room: a block this cap refuses is one nobody
/// has made, and pay-as-you-go pricing refunds what the answer does not use.
const MAX_BLOCK_BYTES: u64 = 1024 * 1024;

/// A deposit the chain holds for a quote, deep enough to decide on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VerifiedDeposit {
    pub token: EvmAddress,
    /// The payer: the account the vault took the funds from.
    pub from: EvmAddress,
    /// What the vault measured as received, which is what it will pay out of.
    pub amount: TokenAmount,
    pub tx_ref: TxHash,
    pub block: BlockNumber,
    /// The hash of the block the log is in, as the log named it: what the block's own
    /// time is read by, so the time is that block's and no other's.
    pub block_hash: [u8; 32],
}

/// Why a chain has no vault to read or send to.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum VaultError {
    #[error("chain {chain_id} has no vault configured")]
    NoVault { chain_id: ChainId },
    #[error("the vault configured for chain {chain_id} is not an address: {reason}")]
    NotAnAddress {
        chain_id: ChainId,
        reason: EvmAddressError,
    },
}

/// Why the read verified no deposit.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum DepositError {
    #[error(transparent)]
    Vault(#[from] VaultError),
    #[error("chain {chain_id} has no chain data young enough to anchor the read on")]
    StaleChainData { chain_id: ChainId },
    #[error(
        "reading from block {from} to the head at {anchor} takes {windows} windows of \
         {LOGS_WINDOW_BLOCKS} blocks, above the cap of {cap}"
    )]
    RangeTooWide {
        from: BlockNumber,
        anchor: BlockNumber,
        windows: u64,
        cap: u64,
    },
    #[error(transparent)]
    NoDepth(#[from] NoDepth),
    #[error(transparent)]
    Rpc(#[from] RpcError),
    #[error("the head block did not come back as a number")]
    UnreadableHead,
    #[error("the logs did not come back as an array")]
    UnreadableLogs,
    #[error("no deposit for quote {quote_hash} is in the vault's log")]
    NotFound { quote_hash: QuoteHash },
    #[error(
        "the vault's log holds {seen} deposits for quote {quote_hash}, and none of them is \
         the one wanted"
    )]
    NoneMatches { quote_hash: QuoteHash, seen: u64 },
    #[error("the deposit in block {block} is not {depth} deep against a head at {latest}")]
    NotConfirmed {
        block: BlockNumber,
        latest: BlockNumber,
        depth: BlockDepth,
    },
    #[error("the provider holds no block {block} under the hash the deposit's log named")]
    NoBlock { block: BlockNumber },
    #[error("the block did not come back as the one asked for, with a time")]
    UnreadableBlock,
}

impl DepositError {
    /// Whether the read found nothing at all it wants in its range: no deposit for the
    /// quote, or none of the one wanted. Not a deposit short of the depth, and not a read
    /// that could not be made, both of which say nothing about a wider range.
    pub fn found_nothing(&self) -> bool {
        match self {
            Self::NotFound { .. } | Self::NoneMatches { .. } => true,
            Self::Vault(_)
            | Self::StaleChainData { .. }
            | Self::RangeTooWide { .. }
            | Self::NoDepth(_)
            | Self::Rpc(_)
            | Self::UnreadableHead
            | Self::UnreadableLogs
            | Self::NotConfirmed { .. }
            | Self::NoBlock { .. }
            | Self::UnreadableBlock => false,
        }
    }
}

/// The amount a read is looking for: exactly the quote's, for the deposit that creates a
/// swap, or at least the least the user was quoted, for the rail's fill on the
/// destination.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WantedAmount {
    Exactly(TokenAmount),
    AtLeast(TokenAmount),
}

/// What a read is looking for among the logs the quote's hash names: a deposit of `token`
/// in `amount`. Any other log under the hash is somebody else's and decides nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Wanted {
    pub token: EvmAddress,
    pub amount: WantedAmount,
}

impl Wanted {
    /// Whether `deposit` is the one wanted.
    pub fn admits(&self, deposit: &VerifiedDeposit) -> bool {
        if deposit.token != self.token {
            return false;
        }
        match self.amount {
            WantedAmount::Exactly(amount) => deposit.amount == amount,
            WantedAmount::AtLeast(least) => deposit.amount >= least,
        }
    }
}

/// Where a read starts, fixed once the provider's own head is known.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Start {
    /// The whole lookback behind the anchor: a claim for a quote with no registration
    /// height, a controller's claim of one the store never held among them.
    Lookback,
    /// A claim for a quote registered at this height: [`FLOOR_MARGIN_BLOCKS`] below the
    /// lower of it and the provider's head. The height is the watcher's, and a watcher can
    /// run ahead of the chain, so it narrows the read and never floors it past the chain.
    Registered(BlockNumber),
    /// The newest window behind the anchor: the engine's arrival read while it waits.
    NewestWindow,
}

/// One read: the vault of `chain_id` for the deposit `wanted` under `quote_hash`, from
/// `start`, and on through the rest of the bounded lookback when `widen` and nothing it
/// wants turns up from `start`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DepositRead {
    pub chain_id: ChainId,
    pub quote_hash: QuoteHash,
    pub wanted: Wanted,
    pub start: Start,
    pub widen: bool,
}

/// Where the read starts: `lookback` blocks ending at `anchor`, the anchor's own block
/// among them, and genesis when the chain is younger than that.
pub fn range_from(anchor: BlockNumber, lookback: DepositLookback) -> BlockNumber {
    BlockNumber::new(
        anchor
            .get()
            .saturating_add(1)
            .saturating_sub(u64::from(lookback.get())),
    )
}

/// One `eth_getLogs` range: `from` to `to`, and to the head for the newest window, so a
/// deposit the watcher's reading has not reached yet is still seen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    pub from: BlockNumber,
    pub to: Option<BlockNumber>,
}

/// The windows that tile `from` to `anchor`, oldest first, the newest open at the head:
/// the deposit that counts is the oldest deep one in range, so the first window that
/// holds one ends the walk. More than [`MAX_LOGS_WINDOWS`] of them is refused by name.
pub fn windows(from: BlockNumber, anchor: BlockNumber) -> Result<Vec<Window>, DepositError> {
    tile(from, anchor, true)
}

/// The windows that tile `from` to `to`, oldest first, every one of them closed: the part
/// of the lookback older than a read's start, which ends where that read began.
pub fn older_windows(from: BlockNumber, to: BlockNumber) -> Result<Vec<Window>, DepositError> {
    tile(from, to, false)
}

/// Tiles `from` to `to` back from `to` in windows of [`LOGS_WINDOW_BLOCKS`], so the newest
/// is whole, and hands them back oldest first, the newest open at the head when `open`.
fn tile(from: BlockNumber, to: BlockNumber, open: bool) -> Result<Vec<Window>, DepositError> {
    let span = to.get().saturating_sub(from.get()).saturating_add(1);
    let count = span.div_ceil(LOGS_WINDOW_BLOCKS);
    if count > MAX_LOGS_WINDOWS {
        return Err(DepositError::RangeTooWide {
            from,
            anchor: to,
            windows: count,
            cap: MAX_LOGS_WINDOWS,
        });
    }
    let mut windows = Vec::with_capacity(count as usize);
    let mut end = to.get();
    for newest in [true].into_iter().chain(std::iter::repeat(false)) {
        let start = end
            .saturating_add(1)
            .saturating_sub(LOGS_WINDOW_BLOCKS)
            .max(from.get());
        windows.push(Window {
            from: BlockNumber::new(start),
            to: (!(newest && open)).then_some(BlockNumber::new(end)),
        });
        if start == from.get() {
            break;
        }
        end = start - 1;
    }
    // tiled back from the end, so the newest is whole; read forward from the oldest
    windows.reverse();
    Ok(windows)
}

/// Whether a window asks for blocks the provider does not have yet: a range starting
/// above its head. Such a range holds nothing, and geth-family providers refuse it rather
/// than answer empty, so it is never sent and counts as a window that found nothing.
pub fn above_head(window: &Window, head: BlockNumber) -> bool {
    window.from > head
}

/// The ranges one read walks, fixed by the provider's head before any window is built.
///
/// The anchor is the lower of the watcher's fresh reading and the provider's head: either
/// may run ahead of the chain, and a window past what the provider holds is one it
/// refuses or answers empty. The lookback ends at the anchor. The read's own start is
/// never before the lookback's, so the narrow read and the rest of the lookback are two
/// ranges that do not overlap, and the rest is read only when it is not empty.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    pub head: BlockNumber,
    pub anchor: BlockNumber,
    /// Where the read from its start begins.
    pub from: BlockNumber,
    /// Where the lookback begins: the rest of the lookback is `lookback_from` to `from`.
    pub lookback_from: BlockNumber,
}

impl Plan {
    pub fn new(
        start: Start,
        watcher: BlockNumber,
        head: BlockNumber,
        lookback: DepositLookback,
    ) -> Self {
        let anchor = watcher.min(head);
        let lookback_from = range_from(anchor, lookback);
        let from = match start {
            Start::Lookback => lookback_from,
            Start::Registered(height) => {
                BlockNumber::new(height.min(head).get().saturating_sub(FLOOR_MARGIN_BLOCKS))
            }
            Start::NewestWindow => range_from(anchor, ONE_WINDOW),
        }
        .max(lookback_from);
        Self {
            head,
            anchor,
            from,
            lookback_from,
        }
    }

    /// The windows of the read from its start, the newest open at the head.
    pub fn narrow(&self) -> Result<Vec<Window>, DepositError> {
        windows(self.from, self.anchor)
    }

    /// The windows of the rest of the lookback, older than the start: none when the start
    /// is the lookback's own.
    pub fn rest(&self) -> Result<Vec<Window>, DepositError> {
        if self.from <= self.lookback_from {
            return Ok(Vec::new());
        }
        older_windows(self.lookback_from, BlockNumber::new(self.from.get() - 1))
    }
}

/// The call that reads one window: the vault's `Deposited` logs naming `quote_hash` from
/// the window's first block to its last, or to the head for the newest.
fn logs_call(vault: EvmAddress, quote_hash: QuoteHash, window: &Window) -> (&'static str, Value) {
    let to_block = match window.to {
        Some(to) => format!("0x{:x}", to.get()),
        None => "latest".to_string(),
    };
    (
        "eth_getLogs",
        json!([{
            "address": hex0x(vault.as_bytes()),
            "topics": [hex0x(&deposited_topic()), hex0x(quote_hash.as_ref())],
            "fromBlock": format!("0x{:x}", window.from.get()),
            "toBlock": to_block,
        }]),
    )
}

/// The twenty low bytes of an indexed address word, when the word is one: the twelve high
/// bytes of an address topic are zero.
fn address_of_word(value: Option<&Value>) -> Option<EvmAddress> {
    let word = parse_hash32(value)?;
    let (padding, address) = word.split_at(12);
    if padding.iter().any(|byte| *byte != 0) {
        return None;
    }
    Some(EvmAddress::new(
        address
            .try_into()
            .expect("BUG: a 32-byte word less 12 bytes is 20"),
    ))
}

/// The deposit `value` records, if it is the vault's `Deposited` log for `quote_hash`, in a
/// block, and not one the provider has since removed. Anything else decides nothing.
fn parse_deposit(
    value: &Value,
    vault: EvmAddress,
    quote_hash: QuoteHash,
) -> Option<VerifiedDeposit> {
    if value.get("removed").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let logged_by: EvmAddress = value.get("address")?.as_str()?.parse().ok()?;
    if logged_by != vault {
        return None;
    }
    let topics = value.get("topics")?.as_array()?;
    if topics.len() != 4 {
        return None;
    }
    if parse_hash32(topics.first())? != deposited_topic() {
        return None;
    }
    if parse_hash32(topics.get(1))? != quote_hash.into_bytes() {
        return None;
    }
    let token = address_of_word(topics.get(2))?;
    let from = address_of_word(topics.get(3))?;
    let amount = TokenAmount::from_be_bytes(parse_hash32(value.get("data"))?);
    let block = parse_block_number(value.get("blockNumber")?.as_str()?)?;
    // a log with no block hash sits in no block: pending, or a provider's half-written
    // record, and neither is a deposit the chain holds
    let block_hash = parse_hash32(value.get("blockHash"))?;
    let tx_ref = TxHash::new(parse_hash32(value.get("transactionHash"))?);
    Some(VerifiedDeposit {
        token,
        from,
        amount,
        tx_ref,
        block,
        block_hash,
    })
}

/// What one window's logs hold of the deposit a read wants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Finding {
    /// The oldest match in the window that is deep enough to decide on.
    Deep(VerifiedDeposit),
    /// Matches, none deep enough against the window's head: the oldest of them.
    Shallow {
        block: BlockNumber,
        latest: BlockNumber,
    },
    /// No match: how many of the quote's deposits into the vault the window holds that are
    /// not the one wanted, none at all included.
    Unmatched { seen: u64 },
}

/// What one answer says: among the logs that are this quote's deposits into `vault`, the
/// oldest that is the deposit `wanted` and `depth` deep against `latest`, else the oldest
/// that is the deposit wanted, else how many were not. Oldest by block, and in the
/// provider's order within a block. The vault marks a quote per payer, so the logs under
/// one hash may be several payers' and anyone can put one ahead of the user's; the others
/// decide nothing, and are counted so a caller can tell stranded funds from no deposit at
/// all.
pub fn find(
    logs: &[Value],
    vault: EvmAddress,
    quote_hash: QuoteHash,
    wanted: &Wanted,
    latest: BlockNumber,
    depth: BlockDepth,
) -> Finding {
    let deposits: Vec<VerifiedDeposit> = logs
        .iter()
        .filter_map(|value| parse_deposit(value, vault, quote_hash))
        .collect();
    let matches = || deposits.iter().filter(|deposit| wanted.admits(deposit));
    // `min_by_key` keeps the first of equals, so a block's deposits stay in log order
    if let Some(deep) = matches()
        .filter(|deposit| is_confirmed(deposit.block, latest, depth))
        .min_by_key(|deposit| deposit.block)
    {
        return Finding::Deep(*deep);
    }
    match matches().min_by_key(|deposit| deposit.block) {
        Some(shallow) => Finding::Shallow {
            block: shallow.block,
            latest,
        },
        None => Finding::Unmatched {
            seen: deposits.len() as u64,
        },
    }
}

/// The verdict over a range, built from its windows' findings oldest window first. The
/// first window holding a deep match holds the oldest deep match in range, so it ends the
/// walk. Until then the walk keeps the oldest match that was not deep enough and counts
/// the logs that matched nothing, so a match not deep enough is the answer only when no
/// window holds a deep one, and a range with no match at all is refused by its count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Walk {
    quote_hash: QuoteHash,
    depth: BlockDepth,
    shallow: Option<(BlockNumber, BlockNumber)>,
    seen: u64,
}

impl Walk {
    pub fn new(quote_hash: QuoteHash, depth: BlockDepth) -> Self {
        Self {
            quote_hash,
            depth,
            shallow: None,
            seen: 0,
        }
    }

    /// Takes the next window's finding, oldest window first: the deposit, when the window
    /// holds one deep enough, which ends the walk.
    pub fn take(&mut self, finding: Finding) -> Option<VerifiedDeposit> {
        match finding {
            Finding::Deep(deposit) => return Some(deposit),
            Finding::Shallow { block, latest } => {
                self.shallow.get_or_insert((block, latest));
            }
            Finding::Unmatched { seen } => self.seen += seen,
        }
        None
    }

    /// Whether the windows taken so far hold nothing the read wants at all: no match,
    /// deep or not. Only then can an older range change the answer.
    pub fn found_nothing(&self) -> bool {
        self.shallow.is_none()
    }

    /// Why the range holds no deposit to decide on, once every window was taken.
    pub fn end(self) -> DepositError {
        let quote_hash = self.quote_hash;
        match (self.shallow, self.seen) {
            (Some((block, latest)), _) => DepositError::NotConfirmed {
                block,
                latest,
                depth: self.depth,
            },
            (None, 0) => DepositError::NotFound { quote_hash },
            (None, seen) => DepositError::NoneMatches { quote_hash, seen },
        }
    }
}

/// The vault the config names for `chain_id`, as an address.
pub fn vault_of(config: &types::Config, chain_id: ChainId) -> Result<EvmAddress, VaultError> {
    let vault = config
        .vault_addresses
        .get(&chain_id)
        .ok_or(VaultError::NoVault { chain_id })?;
    vault
        .as_str()
        .parse()
        .map_err(|reason| VaultError::NotAnAddress { chain_id, reason })
}

/// Reads whether the vault holds the deposit `read` wants, deep enough to decide on.
///
/// First the provider's head, alone, since every window is built from it (see [`Plan`]).
/// Then the range from the read's start to the anchor, and, when `widen` and nothing the
/// read wants is in it, the rest of the lookback before it. Each is walked in windows,
/// oldest first, one outcall each, and a window starting above the head is never sent.
/// The depth is measured against the head read first, which the chain has only passed by
/// the time the logs come back, so a deposit is never taken as deeper than it is. The
/// deposit that counts is the oldest deep match in the whole range (see [`Walk`]), so the
/// first window holding a deep match ends the walk, a match not deep enough yet never
/// hides a deep one in a later window, and a range whose deposit sits in its newest window
/// is read to the end.
// todo_harden_reads: single unreplicated read; upgrade to k-of-n later. Until then the
// answer is bound to the vault this canister configured, the event that vault emits, the
// quote being claimed and the token and amount wanted, and held to the configured depth.
// That binds a log to what this canister asked for and not to the chain: unlike a receipt,
// which must carry a hash this canister signed, a deposit has no hash of ours to be bound
// to, so one provider can invent a deposit as well as delay one. The deploy holds the
// provider to that until the read is replicated or compared across providers.
pub async fn verify_evm_deposit(read: &DepositRead) -> Result<VerifiedDeposit, DepositError> {
    let DepositRead {
        chain_id,
        quote_hash,
        wanted,
        start,
        widen,
    } = *read;
    let config = config::get();
    let vault = vault_of(&config, chain_id)?;
    let now = Timestamp::from_nanos(ic_cdk::api::time());
    let watcher = chain_data::fresh(chain_id, now, config.chain_data_max_age)
        .ok_or(DepositError::StaleChainData { chain_id })?
        .block;
    let depth = confirmations(&config, chain_id)?;
    let head = read_head(chain_id).await?;
    let plan = Plan::new(start, watcher, head, config.deposit_lookback_blocks);
    let reader = Reader {
        chain_id,
        vault,
        quote_hash,
        wanted,
        head,
        depth,
    };
    let mut walk = Walk::new(quote_hash, depth);
    for window in plan.narrow()? {
        if let Some(deposit) = reader.read(&window, &mut walk).await? {
            return Ok(deposit);
        }
    }
    if widen && walk.found_nothing() {
        for window in plan.rest()? {
            if let Some(deposit) = reader.read(&window, &mut walk).await? {
                return Ok(deposit);
            }
        }
    }
    Err(walk.end())
}

/// The provider's own head, asked alone: what every window of the read is built from.
async fn read_head(chain_id: ChainId) -> Result<BlockNumber, DepositError> {
    let answers = rpc::rpc_batch(
        chain_id,
        &[("eth_blockNumber", json!([]))],
        MAX_BLOCK_NUMBER_BYTES,
    )
    .await?;
    answers
        .first()
        .and_then(Value::as_str)
        .and_then(parse_block_number)
        .ok_or(DepositError::UnreadableHead)
}

/// What every window of one read is read against: the chain and its vault, the quote and
/// the deposit wanted, and the head and depth the findings are judged by.
struct Reader {
    chain_id: ChainId,
    vault: EvmAddress,
    quote_hash: QuoteHash,
    wanted: Wanted,
    head: BlockNumber,
    depth: BlockDepth,
}

impl Reader {
    /// Reads one window and hands its finding to the walk: the deposit, when the window
    /// holds one deep enough. A window starting above the head is never sent, and found
    /// nothing.
    async fn read(
        &self,
        window: &Window,
        walk: &mut Walk,
    ) -> Result<Option<VerifiedDeposit>, DepositError> {
        if above_head(window, self.head) {
            return Ok(walk.take(Finding::Unmatched { seen: 0 }));
        }
        let call = logs_call(self.vault, self.quote_hash, window);
        let answers = rpc::rpc_batch(self.chain_id, &[call], MAX_LOGS_BYTES).await?;
        let logs = answers
            .first()
            .and_then(Value::as_array)
            .ok_or(DepositError::UnreadableLogs)?;
        Ok(walk.take(find(
            logs,
            self.vault,
            self.quote_hash,
            &self.wanted,
            self.head,
            self.depth,
        )))
    }
}

/// When the block `deposit` is in was made, by the chain's own clock: the timestamp of the
/// block its log named, read by that block's hash, so the time is that block's and no
/// other's. One outcall.
// todo_harden_reads: single unreplicated read of the provider's block, bound to the hash
// and the number the deposit's log named.
pub async fn landed_at(
    chain_id: ChainId,
    deposit: &VerifiedDeposit,
) -> Result<UnixSeconds, DepositError> {
    let calls = [(
        "eth_getBlockByHash",
        json!([hex0x(&deposit.block_hash), false]),
    )];
    let answers = rpc::rpc_batch(chain_id, &calls, MAX_BLOCK_BYTES).await?;
    block_time(answers.first(), deposit)
}

/// The time a block answer says `deposit`'s block was made, once the answer is that block:
/// its hash and its number the ones the log named. A null answer is a block the provider
/// does not hold, reorged away or not reached yet, and is refused by name.
pub fn block_time(
    answer: Option<&Value>,
    deposit: &VerifiedDeposit,
) -> Result<UnixSeconds, DepositError> {
    let block = answer.ok_or(DepositError::UnreadableBlock)?;
    if block.is_null() {
        return Err(DepositError::NoBlock {
            block: deposit.block,
        });
    }
    let hash = parse_hash32(block.get("hash"));
    let number = block
        .get("number")
        .and_then(Value::as_str)
        .and_then(parse_block_number);
    if hash != Some(deposit.block_hash) || number != Some(deposit.block) {
        return Err(DepositError::UnreadableBlock);
    }
    block
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(|text| text.strip_prefix("0x"))
        .and_then(|hex| u64::from_str_radix(hex, 16).ok())
        .map(UnixSeconds::new)
        .ok_or(DepositError::UnreadableBlock)
}
