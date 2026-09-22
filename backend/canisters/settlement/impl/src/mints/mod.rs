//! The mint read: what a CCTP mint delivered to the destination vault, read off the mint's
//! own receipt. `PaidInStable` records this amount and never the one the burn promised,
//! because the fee Circle takes is the attestation service's to decide within the ceiling
//! the burn set, and only the chain says what arrived.
//!
//! The answer is bound to what this canister asked for: the receipt must be the mint's
//! own (a hash this canister signed), successful, deep enough against a head from the
//! same answer, and the log that counts must be the token messenger's `MintAndWithdraw`
//! naming the destination vault and its USDC.

#[cfg(test)]
mod tests;

use crate::deposits::{vault_of, VaultError};
use crate::rpc::{self, hex0x, parse_block_number, parse_hash32, RpcError, MAX_BLOCK_NUMBER_BYTES};
use crate::storage::config;
use crate::tx::{confirmations, MAX_RECEIPT_BYTES_PER_ITEM};
use serde_json::{json, Value};
use thiserror::Error;
use types::abi::mint_and_withdraw_topic;
use types::tx::is_confirmed;
use types::{BlockDepth, BlockNumber, ChainId, EvmAddress, TokenAmount, TxHash};

/// What one mint delivered: the amount minted to the vault, the fee Circle kept, and the
/// block it landed in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Minted {
    pub amount: TokenAmount,
    pub fee: TokenAmount,
    pub block: BlockNumber,
}

/// The mint a read is looking for: the receipt of `tx_hash`, carrying `messenger`'s log
/// of a mint to `recipient` in `token`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MintOf {
    pub tx_hash: TxHash,
    pub messenger: EvmAddress,
    pub recipient: EvmAddress,
    pub token: EvmAddress,
}

/// Why the read learned nothing of what the mint delivered.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum MintError {
    #[error("no CCTP token messenger is configured")]
    NoTokenMessenger,
    #[error("chain {chain_id} has no USDC address configured")]
    NoUsdc { chain_id: ChainId },
    #[error(transparent)]
    Vault(#[from] VaultError),
    #[error(transparent)]
    Rpc(#[from] RpcError),
    #[error("the head block did not come back as a number")]
    UnreadableHead,
    #[error("the receipt is of {found}, and the mint is {wanted}")]
    AnotherTransaction { wanted: TxHash, found: TxHash },
    #[error("the provider has not mined the mint")]
    Unmined,
    #[error("the mint reverted")]
    Reverted,
    #[error("the mint's receipt {tx_hash} carries no mint to the vault in its USDC")]
    NoMintLog { tx_hash: TxHash },
    #[error("the mint in block {block} is not {depth} deep against a head at {latest}")]
    NotConfirmed {
        block: BlockNumber,
        latest: BlockNumber,
        depth: BlockDepth,
    },
}

/// The batch: the head block, then the receipt of the mint, so the depth is measured
/// against a height from the same answer.
fn read_calls(tx_hash: TxHash) -> Vec<(&'static str, Value)> {
    vec![
        ("eth_blockNumber", json!([])),
        (
            "eth_getTransactionReceipt",
            json!([hex0x(tx_hash.as_ref())]),
        ),
    ]
}

/// The twenty low bytes of an indexed address word, when the word is one.
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

/// The delivery `value` logs, if it is `messenger`'s `MintAndWithdraw` to `recipient` in
/// `token`, with its two amounts. Anything else decides nothing.
fn parse_mint_log(value: &Value, wanted: &MintOf) -> Option<(TokenAmount, TokenAmount)> {
    let logged_by: EvmAddress = value.get("address")?.as_str()?.parse().ok()?;
    if logged_by != wanted.messenger {
        return None;
    }
    let topics = value.get("topics")?.as_array()?;
    if topics.len() != 3 {
        return None;
    }
    if parse_hash32(topics.first())? != mint_and_withdraw_topic() {
        return None;
    }
    if address_of_word(topics.get(1))? != wanted.recipient {
        return None;
    }
    if address_of_word(topics.get(2))? != wanted.token {
        return None;
    }
    let data = hex::decode(value.get("data")?.as_str()?.strip_prefix("0x")?).ok()?;
    let (amount, fee) = data.split_first_chunk::<32>()?;
    let fee: [u8; 32] = fee.try_into().ok()?;
    Some((
        TokenAmount::from_be_bytes(*amount),
        TokenAmount::from_be_bytes(fee),
    ))
}

/// What one answer says about the mint: the delivery its receipt logs, provided the
/// receipt is the mint's own, mined, successful and `depth` deep against `latest`.
pub fn decide(
    receipt: &Value,
    wanted: &MintOf,
    latest: BlockNumber,
    depth: BlockDepth,
) -> Result<Minted, MintError> {
    let read = || -> Option<(TxHash, BlockNumber, bool, &Vec<Value>)> {
        let tx_hash = TxHash::new(parse_hash32(receipt.get("transactionHash"))?);
        let block = parse_block_number(receipt.get("blockNumber")?.as_str()?)?;
        parse_hash32(receipt.get("blockHash"))?;
        let success = receipt.get("status")?.as_str()? == "0x1";
        let logs = receipt.get("logs")?.as_array()?;
        Some((tx_hash, block, success, logs))
    };
    let (tx_hash, block, success, logs) = read().ok_or(MintError::Unmined)?;
    if tx_hash != wanted.tx_hash {
        return Err(MintError::AnotherTransaction {
            wanted: wanted.tx_hash,
            found: tx_hash,
        });
    }
    if !success {
        return Err(MintError::Reverted);
    }
    if !is_confirmed(block, latest, depth) {
        return Err(MintError::NotConfirmed {
            block,
            latest,
            depth,
        });
    }
    let (amount, fee) = logs
        .iter()
        .find_map(|value| parse_mint_log(value, wanted))
        .ok_or(MintError::NoMintLog { tx_hash })?;
    Ok(Minted { amount, fee, block })
}

/// Reads what the mint `tx_hash` on `chain_id` delivered to that chain's vault in its
/// USDC, deep enough to decide on.
// todo_harden_reads: single unreplicated read; upgrade to k-of-n later. Until then the
// answer is bound to a hash this canister signed, the token messenger it configured, the
// vault and the USDC it configured, and held to the configured depth, so one provider can
// delay the read but cannot make the mint deliver what it did not.
pub async fn read_mint(chain_id: ChainId, tx_hash: TxHash) -> Result<Minted, MintError> {
    let config = config::get();
    let wanted = MintOf {
        tx_hash,
        messenger: config.token_messenger.ok_or(MintError::NoTokenMessenger)?,
        recipient: vault_of(&config, chain_id)?,
        token: config
            .usdc_addresses
            .get(chain_id)
            .ok_or(MintError::NoUsdc { chain_id })?,
    };
    let depth = confirmations(&config, chain_id);
    let calls = read_calls(tx_hash);
    // the whole batch or nothing: a decision needs both reads
    let answers = rpc::rpc_batch(
        chain_id,
        &calls,
        MAX_BLOCK_NUMBER_BYTES + MAX_RECEIPT_BYTES_PER_ITEM,
    )
    .await?;
    let latest = answers
        .first()
        .and_then(Value::as_str)
        .and_then(parse_block_number)
        .ok_or(MintError::UnreadableHead)?;
    let receipt = answers.get(1).ok_or(MintError::Unmined)?;
    decide(receipt, &wanted, latest, depth)
}
