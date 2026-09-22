//! The deposit read: whether a user's funds for a quote have arrived in a chain's vault,
//! read from the chain and held to the configured depth. It is the read `claim_swap`
//! decides money on, so every answer is bound to what this canister asked for: the vault
//! it configured, the event the vault emits, the quote it is claiming, and a height from
//! the same answer.

#[cfg(test)]
mod tests;

use crate::rpc::{self, parse_block_number, parse_hash32, RpcError};
use crate::storage::{chain_data, config};
use crate::tx::confirmations;
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
/// quote, which is more than the vault marks per payer, and refunded when unused under
/// pay-as-you-go pricing.
const MAX_LOGS_BYTES: u64 = 32 * 1024;

/// The most an `eth_blockNumber` reply may be: a hex height and its JSON-RPC envelope.
const MAX_BLOCK_NUMBER_BYTES: u64 = 512;

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
    #[error(transparent)]
    Rpc(#[from] RpcError),
    #[error("the head block did not come back as a number")]
    UnreadableHead,
    #[error("the logs did not come back as an array")]
    UnreadableLogs,
    #[error("no deposit for quote {quote_hash} is in the vault's log")]
    NotFound { quote_hash: QuoteHash },
    #[error("the deposit in block {block} is not {depth} deep against a head at {latest}")]
    NotConfirmed {
        block: BlockNumber,
        latest: BlockNumber,
        depth: BlockDepth,
    },
}

/// Where the read starts: `lookback` blocks back from `anchor`, and genesis when the chain
/// is younger than that.
pub fn range_from(anchor: BlockNumber, lookback: DepositLookback) -> BlockNumber {
    BlockNumber::new(anchor.get().saturating_sub(u64::from(lookback.get())))
}

/// `0x` and the hex of `bytes`, which is how a chain takes a topic or a word.
fn hex0x(bytes: &[u8]) -> String {
    format!("0x{}", hex::encode(bytes))
}

/// The batch: the head block, then the vault's `Deposited` logs naming `quote_hash` from
/// `from_block` to the head. One request, so the depth is measured against a height from
/// the same answer.
fn read_calls(
    vault: EvmAddress,
    quote_hash: QuoteHash,
    from_block: BlockNumber,
) -> Vec<(&'static str, Value)> {
    vec![
        ("eth_blockNumber", json!([])),
        (
            "eth_getLogs",
            json!([{
                "address": hex0x(vault.as_bytes()),
                "topics": [hex0x(&deposited_topic()), hex0x(quote_hash.as_ref())],
                "fromBlock": format!("0x{:x}", from_block.get()),
                "toBlock": "latest",
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

/// What one answer says: the first log that is this quote's deposit into `vault`, provided
/// it is `depth` deep against `latest`. Two payers can deposit against one quote (the
/// vault marks a quote per payer), and the first the chain logged is the one the swap is
/// for.
pub fn decide(
    logs: &[Value],
    vault: EvmAddress,
    quote_hash: QuoteHash,
    latest: BlockNumber,
    depth: BlockDepth,
) -> Result<VerifiedDeposit, DepositError> {
    let deposit = logs
        .iter()
        .find_map(|value| parse_deposit(value, vault, quote_hash))
        .ok_or(DepositError::NotFound { quote_hash })?;
    if !is_confirmed(deposit.block, latest, depth) {
        return Err(DepositError::NotConfirmed {
            block: deposit.block,
            latest,
            depth,
        });
    }
    Ok(deposit)
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

/// Reads whether the vault on `chain_id` holds a deposit for `quote_hash`, deep enough to
/// decide on.
///
/// The range starts a bounded lookback (`deposit_lookback_blocks`) behind the head the
/// watcher last pushed, and ends at the provider's head; the depth is measured against
/// that head, asked for in the same batch, so no receipt is judged against a moment other
/// than its own.
// todo_harden_reads: single unreplicated read; upgrade to k-of-n later. Until then the
// answer is bound to the vault this canister configured, the event that vault emits and
// the quote being claimed, and held to the configured depth, so one provider can delay a
// claim but cannot invent a deposit that is not in the vault's log.
pub async fn verify_evm_deposit(
    chain_id: ChainId,
    quote_hash: QuoteHash,
) -> Result<VerifiedDeposit, DepositError> {
    let config = config::get();
    let vault = vault_of(&config, chain_id)?;
    let now = Timestamp::from_nanos(ic_cdk::api::time());
    let anchor = chain_data::fresh(chain_id, now, config.chain_data_max_age)
        .ok_or(DepositError::StaleChainData { chain_id })?
        .block;
    let from_block = range_from(anchor, config.deposit_lookback_blocks);
    let depth = confirmations(&config, chain_id);
    let calls = read_calls(vault, quote_hash, from_block);
    // the whole batch or nothing: a decision needs both reads
    let answers = rpc::rpc_batch(chain_id, &calls, MAX_BLOCK_NUMBER_BYTES + MAX_LOGS_BYTES).await?;
    let latest = answers
        .first()
        .and_then(Value::as_str)
        .and_then(parse_block_number)
        .ok_or(DepositError::UnreadableHead)?;
    let logs = answers
        .get(1)
        .and_then(Value::as_array)
        .ok_or(DepositError::UnreadableLogs)?;
    decide(logs, vault, quote_hash, latest, depth)
}
