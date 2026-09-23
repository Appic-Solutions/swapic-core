//! The deposit read: whether a user's funds for a quote have arrived in a chain's vault,
//! read from the chain and held to the configured depth. It is the read `claim_swap`
//! decides money on, so every answer is bound to what this canister asked for: the vault
//! it configured, the event the vault emits, the quote it is claiming, the token and the
//! amount it is looking for, and a height from the same answer.
//!
//! The vault marks a quote per payer, so anyone can log a deposit under a quote's hash
//! ahead of the user's. The deposit that counts is therefore the one that matches what
//! the read wants, among every log the hash names, and not the first one logged. Among
//! several that match, it is the oldest one in the whole range that is deep enough to
//! decide on, wherever the read's windows fall; a match not deep enough yet is reported
//! only when the range holds no deep one.

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
            | Self::NotConfirmed { .. } => false,
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

/// One read: the vault of `chain_id` for the deposit `wanted` under `quote_hash`, no
/// earlier than `not_before` when the caller knows the deposit cannot precede a block (the
/// quote's registration), and the bounded lookback otherwise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DepositRead {
    pub chain_id: ChainId,
    pub quote_hash: QuoteHash,
    pub wanted: Wanted,
    pub not_before: Option<BlockNumber>,
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
    let span = anchor.get().saturating_sub(from.get()).saturating_add(1);
    let count = span.div_ceil(LOGS_WINDOW_BLOCKS);
    if count > MAX_LOGS_WINDOWS {
        return Err(DepositError::RangeTooWide {
            from,
            anchor,
            windows: count,
            cap: MAX_LOGS_WINDOWS,
        });
    }
    let mut windows = Vec::with_capacity(count as usize);
    let mut to = anchor.get();
    for newest in [true].into_iter().chain(std::iter::repeat(false)) {
        let start = to
            .saturating_add(1)
            .saturating_sub(LOGS_WINDOW_BLOCKS)
            .max(from.get());
        windows.push(Window {
            from: BlockNumber::new(start),
            to: (!newest).then_some(BlockNumber::new(to)),
        });
        if start == from.get() {
            break;
        }
        to = start - 1;
    }
    // tiled back from the anchor, so the newest is whole; read forward from the oldest
    windows.reverse();
    Ok(windows)
}

/// The batch for one window: the head block, then the vault's `Deposited` logs naming
/// `quote_hash` over the window. One request, so the depth is measured against a height
/// from the same answer.
fn read_calls(
    vault: EvmAddress,
    quote_hash: QuoteHash,
    window: &Window,
) -> Vec<(&'static str, Value)> {
    let to_block = match window.to {
        Some(to) => format!("0x{:x}", to.get()),
        None => "latest".to_string(),
    };
    vec![
        ("eth_blockNumber", json!([])),
        (
            "eth_getLogs",
            json!([{
                "address": hex0x(vault.as_bytes()),
                "topics": [hex0x(&deposited_topic()), hex0x(quote_hash.as_ref())],
                "fromBlock": format!("0x{:x}", window.from.get()),
                "toBlock": to_block,
            }]),
        ),
    ]
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
    parse_hash32(value.get("blockHash"))?;
    let tx_ref = TxHash::new(parse_hash32(value.get("transactionHash"))?);
    Some(VerifiedDeposit {
        token,
        from,
        amount,
        tx_ref,
        block,
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
/// The range starts a bounded lookback (`deposit_lookback_blocks`) behind the head the
/// watcher last pushed, or at the block the read says the deposit cannot precede when that
/// is later, and ends at the provider's head. It is walked in windows, oldest first, one
/// outcall each, and the depth is measured against the head asked for in the same batch
/// as the logs it decides on, so no deposit is judged against a moment other than its
/// own. The deposit that counts is the oldest deep match in the whole range (see
/// [`Walk`]), so the first window holding a deep match ends the walk, a match not deep
/// enough yet never hides a deep one in a later window, and a range whose deposit sits in
/// its newest window is read to the end.
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
        not_before,
    } = *read;
    let config = config::get();
    let vault = vault_of(&config, chain_id)?;
    let now = Timestamp::from_nanos(ic_cdk::api::time());
    let anchor = chain_data::fresh(chain_id, now, config.chain_data_max_age)
        .ok_or(DepositError::StaleChainData { chain_id })?
        .block;
    let from = not_before
        .into_iter()
        .chain([range_from(anchor, config.deposit_lookback_blocks)])
        .max()
        .expect("BUG: the lookback is always a start");
    let depth = confirmations(&config, chain_id)?;
    let mut walk = Walk::new(quote_hash, depth);
    for window in windows(from, anchor)? {
        let calls = read_calls(vault, quote_hash, &window);
        // the whole batch or nothing: a decision needs both reads
        let answers =
            rpc::rpc_batch(chain_id, &calls, MAX_BLOCK_NUMBER_BYTES + MAX_LOGS_BYTES).await?;
        let latest = answers
            .first()
            .and_then(Value::as_str)
            .and_then(parse_block_number)
            .ok_or(DepositError::UnreadableHead)?;
        let logs = answers
            .get(1)
            .and_then(Value::as_array)
            .ok_or(DepositError::UnreadableLogs)?;
        if let Some(deposit) = walk.take(find(logs, vault, quote_hash, &wanted, latest, depth)) {
            return Ok(deposit);
        }
    }
    Err(walk.end())
}
