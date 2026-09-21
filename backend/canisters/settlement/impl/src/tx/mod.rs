//! The send path, and the rules A4 to A6 that make it safe.
//!
//! One transaction is created, signed and sent in that order, and the order is the whole
//! point:
//!
//! - `TxCreated` allocates the nonce and is appended with NO await before it (A1), so two
//!   calls that interleave at the signature cannot come back to the same number (A4).
//! - The signature is asked for after the allocation, and `TxSigned` records the exact
//!   bytes before anything is broadcast (A6), so a rebroadcast re-sends what the log
//!   holds and never something else.
//! - An allocated nonce is never abandoned (A5): a transaction that does not land is
//!   re-sent as it is, and then replaced at the same nonce with a higher fee.

#[cfg(test)]
mod tests;

use crate::ecdsa::{self, EcdsaError};
use crate::guards::require_not_halted;
use crate::rpc::{self, RpcError};
use crate::storage::events::{append_event, read_state, AppendError};
use crate::storage::{chain_data, config, outbox};
use crate::task_manager;
use serde_json::{json, Value};
use settlement_api::types::errors::GuardError;
use std::time::Duration;
use thiserror::Error;
use types::events::TxPurpose;
use types::tx::is_confirmed;
use types::{
    Attempt, BlockNumber, ChainId, Eip1559Tx, EventType, EvmAddress, GasAmount, Nonce, OutboxEntry,
    OutboxKey, OutboxStatus, QuoteHash, Timestamp, TxHash, Wei, WeiPerGas,
};

/// How long the current bytes stay out before they are handed to a provider again. A
/// rebroadcast costs one call and fixes the common case, which is a provider that dropped
/// the transaction from its mempool.
const REBROADCAST_AFTER: Duration = Duration::from_secs(30);

/// How long a transaction stays out before it is replaced at a higher fee. Long enough
/// that a chain running normally lands it first, short enough that a swap is not held by
/// an underpriced transaction. A config knob when the engine lands.
const STUCK_AFTER: Duration = Duration::from_secs(120);

/// The most a provider's answer to one batch may be: a receipt is a few hundred bytes of
/// logs at worst, and the cap is what the outcall reserves against.
const MAX_RECEIPT_BYTES: u64 = 32_768;

/// The most a batch of broadcasts may answer: a transaction hash each, or an error.
const MAX_SEND_BYTES: u64 = 8_192;

/// Why no transaction was created, or why a pass could not finish one.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum TxError {
    #[error("the canister is halted, or the caller is not allowed to send: {0:?}")]
    Guard(GuardError),
    #[error("chain {chain_id} has no chain data young enough to price a transaction with")]
    StaleChainData { chain_id: ChainId },
    #[error("the fee for chain {chain_id} does not fit in 256 bits")]
    FeeOutOfRange { chain_id: ChainId },
    #[error("{0} names no swap, and a transaction is signed against a swap's attempt")]
    PurposeNeedsASwap(&'static str),
    #[error("swap {quote_hash} has used every attempt number there is")]
    NoAttemptLeft { quote_hash: QuoteHash },
    #[error(transparent)]
    Append(#[from] AppendError),
    #[error(transparent)]
    Ecdsa(#[from] EcdsaError),
}

/// The fees to send at, from the chain data the watcher pushed: the tip it suggests, and a
/// ceiling of twice the base fee on top of it, which carries a transaction through several
/// blocks of a rising base fee.
fn fees(chain_id: ChainId, now: Timestamp) -> Result<(WeiPerGas, WeiPerGas), TxError> {
    let max_age = config::get().chain_data_max_age;
    let data =
        chain_data::fresh(chain_id, now, max_age).ok_or(TxError::StaleChainData { chain_id })?;
    let max_fee = data
        .base_fee
        .checked_mul(2_u8)
        .and_then(|headroom| headroom.checked_add(data.priority_fee))
        .ok_or(TxError::FeeOutOfRange { chain_id })?;
    Ok((max_fee, data.priority_fee))
}

/// Creates, signs and queues one transaction, and answers the hash it will be looked up
/// by.
///
/// The order is the law of this module: halt check, fees, `TxCreated` with no await before
/// it, signature, `TxSigned`, queue. The broadcast itself is the outbox pass, so the caller
/// does not wait on a provider to know its nonce is safely allocated.
pub async fn create_and_send(
    purpose: TxPurpose,
    chain_id: ChainId,
    to: EvmAddress,
    value: Wei,
    data: Vec<u8>,
    gas_limit: GasAmount,
) -> Result<TxHash, TxError> {
    require_not_halted().map_err(TxError::Guard)?;
    // a cancel spends a nonce without a swap behind it, and `TxSigned` is a swap's line:
    // the swap-less signed record arrives with the engine
    let quote_hash = purpose
        .quote_hash()
        .ok_or(TxError::PurposeNeedsASwap("a cancel"))?;
    let now = Timestamp::from_nanos(ic_cdk::api::time());
    let (max_fee, max_priority_fee) = fees(chain_id, now)?;

    // A1 and A4: from here to the append there is no await, so the number read is the
    // number written
    let nonce = read_state(|state| state.next_nonce(&chain_id));
    append_event(EventType::TxCreated {
        purpose,
        chain_id,
        nonce,
        to,
        value,
        data: data.clone(),
        gas_limit,
        max_fee,
        max_priority_fee,
    })?;

    let tx_data = data.clone();
    let tx = Eip1559Tx {
        chain_id,
        nonce,
        max_fee,
        max_priority_fee,
        gas_limit,
        to,
        value,
        data,
    };
    // the `TxCreated` guard just proved the swap exists and can still sign, so it has a
    // next attempt; `TxSigned` refuses anything else below
    let attempt = read_state(|state| {
        state
            .swap(&quote_hash)
            .ok()
            .and_then(|swap| swap.next_attempt())
    })
    .ok_or(TxError::NoAttemptLeft { quote_hash })?;
    // the signature is the await this whole order exists for
    let signature = ecdsa::sign(tx.signing_hash()).await?;
    let signed = tx.into_signed(signature);
    let tx_hash = signed.hash();
    append_event(EventType::TxSigned {
        quote_hash,
        attempt,
        chain_id,
        tx_hash,
        raw_tx: signed.raw().to_vec(),
    })?;
    outbox::put(OutboxEntry {
        purpose,
        chain_id,
        nonce,
        attempt: Some(attempt),
        hashes: vec![tx_hash],
        raw_tx: signed.raw().to_vec(),
        max_fee,
        max_priority_fee,
        status: OutboxStatus::Queued,
        created_at: now,
        last_sent_at: None,
        to,
        value,
        data: tx_data,
        gas_limit,
    });
    task_manager::outbox::arm();
    Ok(tx_hash)
}

/// `0x` and the hex of `bytes`, which is how a chain takes raw bytes.
fn hex0x(bytes: &[u8]) -> String {
    format!("0x{}", hex::encode(bytes))
}

/// A provider saying it already has this transaction is the same as it accepting it: the
/// bytes are on the network, which is all a broadcast is for.
fn already_on_the_network(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    ["already known", "already imported", "known transaction"]
        .iter()
        .any(|phrase| message.contains(phrase))
}

/// A provider saying the nonce is too low is saying the nonce is already spent on the
/// chain, and this canister is the only account that spends it: one of the transactions
/// this entry has broadcast at that nonce is mined. The entry goes to the receipt reader,
/// which looks up every one of them, rather than sitting in the queue forever.
fn nonce_already_spent(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    ["nonce too low", "nonce is too low", "oldnonce"]
        .iter()
        .any(|phrase| message.contains(phrase))
}

/// Hands every queued transaction to its chain's provider, one batch per chain. An entry a
/// provider refuses stays queued and goes out again on the next pass: the nonce is
/// allocated either way, so the only way out is forward.
pub async fn flush() {
    let now = Timestamp::from_nanos(ic_cdk::api::time());
    let limit = config::get().max_batch_items as usize;
    for chain_id in outbox::chains() {
        let batch = outbox::by_status(chain_id, OutboxStatus::Queued, limit);
        if batch.is_empty() {
            continue;
        }
        let calls: Vec<(&str, Value)> = batch
            .iter()
            .map(|entry| ("eth_sendRawTransaction", json!([hex0x(&entry.raw_tx)])))
            .collect();
        let Ok(answers) = rpc::rpc_batch_each(chain_id, &calls, MAX_SEND_BYTES).await else {
            // the provider is unreachable: every entry stays queued
            continue;
        };
        for (entry, answer) in batch.into_iter().zip(answers) {
            let accepted = match answer {
                Ok(_) => true,
                Err(RpcError::Rpc { message, .. }) => {
                    already_on_the_network(&message) || nonce_already_spent(&message)
                }
                Err(_) => false,
            };
            if accepted {
                // A3: the entry is the one this pass read, and the only field it moves is
                // how far the broadcast got
                if let Some(mut current) = outbox::get(entry.key()) {
                    if current.tx_hash() == entry.tx_hash() {
                        current.sent(now);
                        outbox::put(current);
                    }
                }
            }
        }
    }
}

/// Reads what happened to every transaction still out, one batch per chain: the head
/// block, then a receipt for every hash ever broadcast at each open nonce. A receipt deep
/// enough closes the attempt, a reverted one fails it, and a transaction that is not
/// landing is re-sent and then replaced.
pub async fn check_open() {
    let now = Timestamp::from_nanos(ic_cdk::api::time());
    let config = config::get();
    let limit = config.max_batch_items as usize;
    for chain_id in outbox::chains() {
        let open = outbox::by_status(chain_id, OutboxStatus::Sent, limit);
        if open.is_empty() {
            continue;
        }
        let mut calls: Vec<(&str, Value)> = vec![("eth_blockNumber", json!([]))];
        for entry in &open {
            for hash in &entry.hashes {
                calls.push(("eth_getTransactionReceipt", json!([hex0x(hash.as_ref())])));
            }
        }
        let Ok(answers) = rpc::rpc_batch_each(chain_id, &calls, MAX_RECEIPT_BYTES).await else {
            continue;
        };
        let Some(latest) = answers.first().and_then(|answer| {
            answer
                .as_ref()
                .ok()
                .and_then(|value| value.as_str())
                .and_then(parse_block_number)
        }) else {
            continue;
        };
        let depth = config
            .confirmations
            .get(&chain_id)
            .copied()
            .unwrap_or(types::BlockDepth::new(1));
        let mut next = 1;
        for entry in open {
            let receipts = &answers[next..next + entry.hashes.len()];
            next += entry.hashes.len();
            let landed = receipts
                .iter()
                .filter_map(|answer| answer.as_ref().ok())
                .find_map(parse_receipt);
            match landed {
                Some(Receipt { success: false, .. }) => {
                    close(
                        &entry,
                        EventType::TxFailed {
                            quote_hash: entry
                                .purpose
                                .quote_hash()
                                .expect("BUG: only a swap's transaction reaches the outbox"),
                            attempt: entry.attempt.unwrap_or(Attempt::FIRST),
                            reason: "the transaction reverted on the chain".to_string(),
                        },
                    );
                }
                Some(Receipt { block, tx_hash, .. }) if is_confirmed(block, latest, depth) => {
                    close(
                        &entry,
                        EventType::TxConfirmed {
                            quote_hash: entry
                                .purpose
                                .quote_hash()
                                .expect("BUG: only a swap's transaction reaches the outbox"),
                            attempt: entry.attempt.unwrap_or(Attempt::FIRST),
                            chain_id,
                            tx_hash,
                            block,
                        },
                    );
                }
                // mined but not deep enough: nothing to do but wait
                Some(_) => {}
                None => push_again(&entry, chain_id, now).await,
            }
        }
    }
}

/// Appends the line that closes an attempt and drops the entry. A refused append leaves
/// the entry where it is, so the next pass sees the same receipt and tries again.
fn close(entry: &OutboxEntry, payload: EventType) {
    if append_event(payload).is_ok() {
        outbox::remove(entry.key());
    }
}

/// A transaction that has not landed: the same bytes again while it is young, and a
/// replacement at the same nonce once it is not (A5). Neither abandons the nonce.
async fn push_again(entry: &OutboxEntry, chain_id: ChainId, now: Timestamp) {
    let Some(out_for) = entry.sent_for(now) else {
        return;
    };
    if out_for >= STUCK_AFTER {
        // a replacement is a new transaction, signed and broadcast, so the halt switch
        // gates it like every other path that creates one. Reading receipts and closing
        // attempts carries on while halted: an operator investigating a divergence wants
        // to see what the chains did with what is already out there, and a rebroadcast
        // sends bytes this canister signed before it stopped.
        if require_not_halted().is_err() {
            return;
        }
        replace(entry, chain_id, now).await;
    } else if out_for >= REBROADCAST_AFTER {
        // the same bytes, so no new signature and no new line in the log
        let mut queued = entry.clone();
        queued.status = OutboxStatus::Queued;
        queued.last_sent_at = None;
        outbox::put(queued);
    }
}

/// Re-signs the same transaction at a higher fee and records it, keeping the nonce.
async fn replace(entry: &OutboxEntry, chain_id: ChainId, now: Timestamp) {
    // a replacement a node accepts pays at least an eighth more, so the fee doubles: the
    // fresh chain data is a floor, not the answer, because the old fee may already be
    // above it
    let (max_fee, max_priority_fee) = match bumped(entry, chain_id, now) {
        Some(fees) => fees,
        None => return,
    };
    let tx = Eip1559Tx {
        chain_id,
        nonce: entry.nonce,
        max_fee,
        max_priority_fee,
        gas_limit: entry.gas_limit,
        to: entry.to,
        value: entry.value,
        data: entry.data.clone(),
    };
    let Ok(signature) = ecdsa::sign(tx.signing_hash()).await else {
        return;
    };
    let signed = tx.into_signed(signature);
    let appended = append_event(EventType::TxReplaced {
        purpose: entry.purpose,
        chain_id,
        nonce: entry.nonce,
        max_fee,
        max_priority_fee,
        tx_hash: signed.hash(),
        raw_tx: signed.raw().to_vec(),
    });
    if appended.is_err() {
        return;
    }
    // A3: the entry this pass read is the one replaced, and only if it is still the one
    // the outbox holds
    if outbox::get(entry.key()).is_some_and(|current| current.tx_hash() == entry.tx_hash()) {
        outbox::put(entry.replaced(
            signed.hash(),
            signed.raw().to_vec(),
            max_fee,
            max_priority_fee,
        ));
    }
}

/// The fees a replacement pays: double what the transaction being replaced paid, and never
/// below what the chain is asking now.
fn bumped(
    entry: &OutboxEntry,
    chain_id: ChainId,
    now: Timestamp,
) -> Option<(WeiPerGas, WeiPerGas)> {
    let (fresh_max, fresh_tip) = fees(chain_id, now).ok()?;
    let max_fee = entry.max_fee.checked_mul(2_u8)?.max(fresh_max);
    let max_priority_fee = entry.max_priority_fee.checked_mul(2_u8)?.max(fresh_tip);
    Some((max_fee, max_priority_fee))
}

/// What a receipt says.
struct Receipt {
    tx_hash: TxHash,
    block: BlockNumber,
    success: bool,
}

/// `0x`-prefixed hex as a block number.
fn parse_block_number(text: &str) -> Option<BlockNumber> {
    u64::from_str_radix(text.strip_prefix("0x")?, 16)
        .ok()
        .map(BlockNumber::new)
}

/// A receipt, or nothing at all for the `null` a provider answers for a transaction it has
/// not mined.
fn parse_receipt(value: &Value) -> Option<Receipt> {
    let block = parse_block_number(value.get("blockNumber")?.as_str()?)?;
    let tx_hash = hex::decode(value.get("transactionHash")?.as_str()?.strip_prefix("0x")?).ok()?;
    let status = value.get("status")?.as_str()?;
    Some(Receipt {
        tx_hash: TxHash::new(<[u8; 32]>::try_from(tx_hash).ok()?),
        block,
        success: status == "0x1",
    })
}

/// The entry a swap's open attempt is in, for the engine and for the tests.
pub fn open_entry(chain_id: ChainId, nonce: Nonce) -> Option<OutboxEntry> {
    outbox::get(OutboxKey { chain_id, nonce })
}

/// The swap an outbox entry belongs to, for a caller that has one.
pub fn quote_hash_of(entry: &OutboxEntry) -> Option<QuoteHash> {
    entry.purpose.quote_hash()
}

impl From<TxError> for settlement_api::types::tx::TxError {
    fn from(error: TxError) -> Self {
        match error {
            TxError::Guard(error) => Self::Guard(error),
            TxError::StaleChainData { chain_id } => Self::StaleChainData {
                chain_id: chain_id.get(),
            },
            TxError::FeeOutOfRange { chain_id } => Self::FeeOutOfRange {
                chain_id: chain_id.get(),
            },
            TxError::PurposeNeedsASwap(purpose) => Self::PurposeNeedsASwap {
                purpose: purpose.to_string(),
            },
            TxError::NoAttemptLeft { quote_hash } => Self::NoAttemptLeft {
                quote_hash: quote_hash.into_bytes(),
            },
            TxError::Append(error) => Self::Append(error.into()),
            TxError::Ecdsa(error) => Self::Ecdsa(error.into()),
        }
    }
}
