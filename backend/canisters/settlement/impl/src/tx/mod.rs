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
//!   re-sent as it is, and then replaced at the same nonce with a higher fee. A nonce whose
//!   transaction never came back at all, because the signature was refused or the signed
//!   record was, is held by the fold as created-but-unsigned and spent by [`cancel_stranded`]
//!   with a zero-value self-transfer, so an EVM account this canister owns never has a gap
//!   in its nonces and never stops being able to send.

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
use types::chain_data::{ChainData, Fees, MAX_FEE_PER_GAS};
use types::events::TxPurpose;
use types::tx::is_confirmed;
use types::{
    Attempt, BlockNumber, ChainId, Eip1559Tx, EventType, EvmAddress, GasAmount, Nonce, NonceKey,
    OutboxEntry, OutboxStatus, QuoteHash, Timestamp, TxHash, Wei, WeiPerGas,
};

/// How long the current bytes stay out before they are handed to a provider again. A
/// rebroadcast costs one call and fixes the common case, which is a provider that dropped
/// the transaction from its mempool.
const REBROADCAST_AFTER: Duration = Duration::from_secs(30);

/// How long a transaction stays on the network before it is replaced at a higher fee,
/// counted from the FIRST time its bytes went out. Long enough that a chain running
/// normally lands it first, short enough that a swap is not held by an underpriced
/// transaction. A config knob when the engine lands.
const STUCK_AFTER: Duration = Duration::from_secs(120);

/// The most any one transaction this canister signs may spend on gas: one ether, which is
/// two orders of magnitude above what a settlement transaction on the most congested chain
/// this canister sends to costs, and far below what a broken fee reading would produce.
///
/// The per-gas ceiling and the multiple a replacement may bid are the other two bounds; this
/// one is the product of a price and a limit, so it catches a gas limit nobody bounded as
/// well as a price. Like the other two it is a constant rather than a config knob, so no
/// config write can raise it; the knob belongs with the engine's gas policy in a later plan.
const MAX_TRANSACTION_COST: Wei = Wei::new(1_000_000_000_000_000_000);

/// The most a provider's answer to one batch may be: a receipt is a few hundred bytes of
/// logs at worst, and the cap is what the outcall reserves against.
const MAX_RECEIPT_BYTES: u64 = 32_768;

/// The most a batch of broadcasts may answer: a transaction hash each, or an error.
const MAX_SEND_BYTES: u64 = 8_192;

/// How deep a receipt must be on a chain the config does not list a depth for. One is the
/// safe floor rather than a free pass: `is_confirmed` counts the receipt's own block as the
/// first confirmation, so a depth of one still refuses a receipt from a block the head has
/// not reached, which is a provider answering from two different moments of the chain.
const DEFAULT_CONFIRMATIONS: types::BlockDepth = types::BlockDepth::new(1);

/// Why no transaction was created, or why a pass could not finish one.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum TxError {
    #[error("the canister is halted, or the caller is not allowed to send: {0:?}")]
    Guard(GuardError),
    #[error("chain {chain_id} has no chain data young enough to price a transaction with")]
    StaleChainData { chain_id: ChainId },
    #[error(
        "the fee for chain {chain_id} is outside what this canister will pay: at most \
         {ceiling} per gas"
    )]
    FeeOutOfRange {
        chain_id: ChainId,
        ceiling: WeiPerGas,
    },
    #[error("a transaction on chain {chain_id} would spend {cost} on gas, above the bound of {MAX_TRANSACTION_COST}")]
    GasCostTooHigh { chain_id: ChainId, cost: Wei },
    #[error("{0} names no swap, and a transaction is signed against a swap's attempt")]
    PurposeNeedsASwap(&'static str),
    #[error("swap {quote_hash} has used every attempt number there is")]
    NoAttemptLeft { quote_hash: QuoteHash },
    #[error(transparent)]
    Append(#[from] AppendError),
    #[error(transparent)]
    Ecdsa(#[from] EcdsaError),
}

/// The freshest reading of a chain, or the refusal that prices nothing: a transaction is
/// never sent on a reading the watcher stopped refreshing, because the fee it would carry
/// is the fee some earlier moment of the chain was asking.
fn fresh_reading(chain_id: ChainId, now: Timestamp) -> Result<ChainData, TxError> {
    let max_age = config::get().chain_data_max_age;
    chain_data::fresh(chain_id, now, max_age).ok_or(TxError::StaleChainData { chain_id })
}

/// The fees to send at, from the chain data the watcher pushed: the tip it suggests, and a
/// ceiling of twice the base fee on top of it, which carries a transaction through several
/// blocks of a rising base fee. A reading that prices a transaction above
/// [`MAX_FEE_PER_GAS`] prices nothing: the bound is what a fee this canister did not compute
/// is held to.
fn fees(chain_id: ChainId, now: Timestamp) -> Result<Fees, TxError> {
    fresh_reading(chain_id, now)?
        .fees()
        .ok_or(TxError::FeeOutOfRange {
            chain_id,
            ceiling: MAX_FEE_PER_GAS,
        })
}

/// Refuses a transaction whose gas could cost more than this canister will ever spend on
/// one. The per-gas ceiling bounds the price and this bounds the price times the limit, so
/// a gas limit nobody checked cannot turn a sane price into an insane bill.
fn affordable(chain_id: ChainId, fees: Fees, gas_limit: GasAmount) -> Result<(), TxError> {
    let cost = fees.worst_cost(gas_limit).ok_or(TxError::GasCostTooHigh {
        chain_id,
        cost: Wei::MAX,
    })?;
    if cost > MAX_TRANSACTION_COST {
        return Err(TxError::GasCostTooHigh { chain_id, cost });
    }
    Ok(())
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
    let fees = fees(chain_id, now)?;
    affordable(chain_id, fees, gas_limit)?;

    // read before the allocation, not after it: a swap that has used every attempt number
    // there is cannot sign anything, and refusing it here costs nothing, while refusing it
    // after the append would leave a number handed out for a transaction that never existed
    let attempt = read_state(|state| {
        state
            .swap(&quote_hash)
            .ok()
            .and_then(|swap| swap.next_attempt())
    })
    .ok_or(TxError::NoAttemptLeft { quote_hash })?;

    // A1 and A4: from here to the append there is no await, so the number read is the
    // number written
    let nonce = read_state(|state| state.next_nonce(&chain_id));
    let tx = Eip1559Tx {
        chain_id,
        nonce,
        max_fee: fees.max_fee(),
        max_priority_fee: fees.max_priority_fee(),
        gas_limit,
        to,
        value,
        data,
    };
    append_event(EventType::TxCreated {
        purpose,
        chain_id,
        nonce,
        to,
        value,
        data: tx.data.clone(),
        gas_limit,
        max_fee: fees.max_fee(),
        max_priority_fee: fees.max_priority_fee(),
    })?;

    // armed here rather than after the queue, and before the first await, so every way
    // this call can fail from now on still leaves a pass scheduled to end the allocation.
    // A trap before the await rolls the append back with it, so an unsigned nonce and an
    // unarmed pass cannot both happen.
    task_manager::outbox::arm();

    // the signature is the await this whole order exists for. From here on a failure
    // leaves the nonce in the fold as created-but-unsigned, and the outbox pass spends it
    // with a cancel rather than leaving the account with a gap (A5).
    let signature = ecdsa::sign(tx.signing_hash()).await?;
    let data = tx.data.clone();
    let signed = tx.into_signed(signature);
    let tx_hash = signed.hash();
    let raw_tx = signed.raw().to_vec();
    append_event(EventType::TxSigned {
        quote_hash,
        attempt,
        chain_id,
        tx_hash,
        raw_tx: raw_tx.clone(),
    })?;
    outbox::put(OutboxEntry {
        purpose,
        chain_id,
        nonce,
        attempt: Some(attempt),
        hashes: vec![tx_hash],
        raw_tx,
        max_fee: fees.max_fee(),
        max_priority_fee: fees.max_priority_fee(),
        status: OutboxStatus::Queued,
        created_at: now,
        last_sent_at: None,
        first_sent_at: None,
        to,
        value,
        data,
        gas_limit,
    });
    task_manager::outbox::arm();
    Ok(tx_hash)
}

/// The gas a bare transfer of the chain's own currency costs, which is what a cancel is.
/// Fixed by the EVM itself, so no config knob can move it.
const CANCEL_GAS_LIMIT: u64 = 21_000;

/// Spends every nonce that was handed out and never signed for, with a zero-value transfer
/// from this canister's address to itself at that exact number (rule A5).
///
/// A nonce leaves the allocator with `TxCreated` and is spent by `TxSigned`. Anything that
/// refuses in between, a rejected signing call, a signed record the swap no longer admits,
/// a trap or an upgrade mid-await, leaves the number handed out with nothing carrying it,
/// and an EVM account with a gap at N can never mine N+1: every payout, refund and burn on
/// that chain stops. So the allocation is in the fold, and this pass ends it.
///
/// The wait before a cancel is one batch window: the whole path from the append to the
/// signed record runs inside one message chain, so an allocation older than a window has
/// lost its transaction. Signing and broadcasting a cancel is creating a transaction, so
/// the halt switch gates it like every other such path.
pub async fn cancel_stranded() {
    if require_not_halted().is_err() {
        return;
    }
    let now = Timestamp::from_nanos(ic_cdk::api::time());
    let window = config::get().batch_window;
    let stranded: Vec<NonceKey> = read_state(|state| {
        state
            .unsigned_nonces()
            .into_iter()
            .filter(|(_, unsigned)| unsigned.is_stranded(now, window))
            .map(|(key, _)| key)
            .collect()
    });
    for key in stranded {
        cancel(key, now).await;
    }
}

/// Signs, records and queues the cancel of one allocated nonce. Recorded before it is
/// broadcast like every other transaction (A6), and the record is what seals the nonce's
/// fate: the entry then rides the outbox as any transaction does, and its receipt needs no
/// further line in the log.
async fn cancel(key: NonceKey, now: Timestamp) {
    let NonceKey { chain_id, nonce } = key;
    let gas_limit = GasAmount::from(CANCEL_GAS_LIMIT);
    // the same door every other send is priced through, so a cancel carries the same
    // ceilings: a bad reading refuses it rather than putting a huge fee on the chain
    let Ok(fees) = fees(chain_id, now) else {
        return;
    };
    if affordable(chain_id, fees, gas_limit).is_err() {
        return;
    }
    let Ok(mine) = ecdsa::canister_address().await else {
        return;
    };
    // the fold may have moved while the address was being read: a `TxSigned` that arrived
    // late spends this number, and cancelling it would put a second transaction on it
    if read_state(|state| state.unsigned_nonces().into_iter().all(|(at, _)| at != key)) {
        return;
    }
    let tx = Eip1559Tx {
        chain_id,
        nonce,
        max_fee: fees.max_fee(),
        max_priority_fee: fees.max_priority_fee(),
        gas_limit,
        to: mine,
        value: Wei::ZERO,
        data: Vec::new(),
    };
    let Ok(signature) = ecdsa::sign(tx.signing_hash()).await else {
        return;
    };
    let signed = tx.into_signed(signature);
    let tx_hash = signed.hash();
    let raw_tx = signed.raw().to_vec();
    if append_event(EventType::TxCancelled {
        chain_id,
        nonce,
        tx_hash,
        raw_tx: raw_tx.clone(),
    })
    .is_err()
    {
        return;
    }
    outbox::put(OutboxEntry {
        purpose: TxPurpose::Cancel(chain_id),
        chain_id,
        nonce,
        attempt: None,
        hashes: vec![tx_hash],
        raw_tx,
        max_fee: fees.max_fee(),
        max_priority_fee: fees.max_priority_fee(),
        status: OutboxStatus::Queued,
        created_at: now,
        last_sent_at: None,
        first_sent_at: None,
        to: mine,
        value: Wei::ZERO,
        data: Vec::new(),
        gas_limit,
    });
    task_manager::outbox::arm();
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
///
/// A provider that says this and is lying self-corrects. The entry then follows the normal
/// path: no receipt comes back, so the bytes go out again after the rebroadcast window and
/// are replaced at a higher fee after the stuck window, and an honest chain refuses that
/// replacement if the nonce really was spent. Nothing here decides money on the string; it
/// decides only which of this entry's own transactions to look up.
fn nonce_already_spent(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    ["nonce too low", "nonce is too low", "oldnonce"]
        .iter()
        .any(|phrase| message.contains(phrase))
}

/// Hands every queued transaction to its chain's provider, one batch per chain. An entry a
/// provider refuses stays queued and goes out again on the next pass: the nonce is
/// allocated either way, so the only way out is forward.
///
/// An entry leaves the queue on three answers, and only one of them is the provider
/// accepting these bytes: a result, a provider that already holds the transaction, and a
/// nonce the chain calls too low, which means one of this entry's EARLIER transactions is
/// already mined. All three mean the same thing for the queue, that there is nothing left
/// to broadcast and the receipt reader takes it from here.
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
            let off_the_queue = match answer {
                Ok(_) => true,
                Err(RpcError::Rpc { message, .. }) => {
                    already_on_the_network(&message) || nonce_already_spent(&message)
                }
                Err(_) => false,
            };
            if off_the_queue {
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
/// enough closes the attempt, a reverted one at the same depth fails it, and a transaction
/// that is not landing is re-sent and then replaced.
///
/// This is the read that decides money: the lines that close an attempt are appended from
/// what it answers.
// todo_harden_reads: single unreplicated read; upgrade to k-of-n later. Until then the
// answers are bound to what this canister signed (`parse_receipt` refuses a receipt for a
// hash this entry never broadcast) and held to the configured depth, so one provider can
// delay a decision but cannot invent one.
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
        let depth = confirmations(&config, chain_id);
        // `rpc_batch_each` answers exactly one result per call, in call order, so the head
        // is at 0 and each entry's receipts follow in the order the calls were built
        let mut receipts_from = 1;
        for entry in open {
            let receipts = &answers[receipts_from..receipts_from + entry.hashes.len()];
            receipts_from += entry.hashes.len();
            let landed = receipts
                .iter()
                .filter_map(|answer| answer.as_ref().ok())
                .find_map(|value| parse_receipt(value, &entry.hashes).ok());
            match landed {
                // rule A10: only a receipt at the configured depth closes an attempt, and
                // that holds however the transaction ended. A reverted receipt closed at
                // once would free the swap to be re-signed at a fresh nonce, and a one-block
                // reorg that drops the revert and includes the replacement then pays twice.
                Some(receipt) if is_confirmed(receipt.block, latest, depth) => {
                    let Receipt {
                        tx_hash,
                        block,
                        success,
                    } = receipt;
                    if success {
                        close(&entry, |quote_hash, attempt| EventType::TxConfirmed {
                            quote_hash,
                            attempt,
                            chain_id,
                            tx_hash,
                            block,
                        });
                    } else {
                        close(&entry, |quote_hash, attempt| EventType::TxFailed {
                            quote_hash,
                            attempt,
                            reason: "the transaction reverted on the chain".to_string(),
                        });
                    }
                }
                // mined but not deep enough: nothing to do but wait
                Some(_) => {}
                None => push_again(&entry, chain_id, now).await,
            }
        }
    }
}

/// How deep a receipt on `chain_id` must be before it closes an attempt.
///
/// A chain the config forgot confirms at [`DEFAULT_CONFIRMATIONS`], which is the safe floor
/// and not a free pass: a receipt still has to be in a block the head has reached.
fn confirmations(config: &types::Config, chain_id: ChainId) -> types::BlockDepth {
    config
        .confirmations
        .get(&chain_id)
        .copied()
        .unwrap_or(DEFAULT_CONFIRMATIONS)
}

/// Drops the entry, appending the line that closes its attempt when it has one. A refused
/// append leaves the entry where it is, so the next pass sees the same receipt and tries
/// again.
///
/// A cancel closes no attempt and needs no line: `TxCancelled` already sealed that nonce's
/// fate before the bytes went out, so what the chain did with them changes nothing in the
/// fold. An entry that names a swap and carries no attempt number is neither shape, so it
/// is left alone rather than closed against a number nobody recorded: the consensus log
/// would otherwise carry an attempt this canister invented.
///
/// A3, the re-read the two other post-await writes in this module do, is not needed here:
/// the entry is being removed, not edited, and the only writer that could have put another
/// entry at this key is `create_and_send`, which only ever writes at a number the allocator
/// has just handed out and this one is not.
fn close(entry: &OutboxEntry, payload: impl FnOnce(QuoteHash, Attempt) -> EventType) {
    match (entry.purpose.quote_hash(), entry.attempt) {
        (None, _) => outbox::remove(entry.key()),
        (Some(quote_hash), Some(attempt)) => {
            if append_event(payload(quote_hash, attempt)).is_ok() {
                outbox::remove(entry.key());
            }
        }
        (Some(_), None) => {}
    }
}

/// A transaction that has not landed: the same bytes again while it is young, and a
/// replacement at the same nonce once it is not (A5). Neither abandons the nonce.
///
/// The two clocks are deliberately different. The rebroadcast runs from the last time the
/// bytes went out, because that is what it repairs: a provider that dropped them. The
/// replacement runs from the FIRST time they went out, because a rebroadcast that reset it
/// would postpone the fee bump every thirty seconds and the transaction would sit
/// underpriced forever.
async fn push_again(entry: &OutboxEntry, chain_id: ChainId, now: Timestamp) {
    let (Some(since_last), Some(since_first)) = (entry.sent_for(now), entry.out_for(now)) else {
        return;
    };
    if since_first >= STUCK_AFTER {
        // a replacement is a new transaction, signed and broadcast, so the halt switch
        // gates it like every other path that creates one, and a halted canister leaves
        // the entry exactly where it is. Reading receipts and closing attempts carries on
        // while halted: an operator investigating a divergence wants to see what the
        // chains did with what is already out there.
        if require_not_halted().is_err() {
            return;
        }
        if replace(entry, chain_id, now).await {
            return;
        }
        // no replacement: already bidding the ceiling, or the reading is too old to price
        // a bid against. The bytes are still the right bytes and the nonce is still this
        // entry's, so they keep going out on the rebroadcast cadence below.
    }
    if since_last >= REBROADCAST_AFTER {
        // the same bytes, so no new signature and no new line in the log
        let mut queued = entry.clone();
        queued.status = OutboxStatus::Queued;
        queued.last_sent_at = None;
        outbox::put(queued);
    }
}

/// Re-signs the same transaction at a higher fee and records it, keeping the nonce, and
/// answers whether it did.
///
/// A replacement signs new bytes and broadcasts them, so callers gate it on
/// `require_not_halted`. It answers `false` when the fee cannot be raised: the entry is
/// already bidding the ceiling [`ChainData::fee_ceiling`] allows, or the chain reading is
/// too old to price against, or a call failed. None of those abandon the nonce; the current
/// bytes stay on the network.
async fn replace(entry: &OutboxEntry, chain_id: ChainId, now: Timestamp) -> bool {
    // a replacement a node accepts pays at least an eighth more, so the fee doubles: the
    // fresh chain data is a floor, not the answer, because the old fee may already be
    // above it, and the ceiling is what stops the doubling from running away
    let Some(fees) = bumped(entry, chain_id, now) else {
        return false;
    };
    let tx = Eip1559Tx {
        chain_id,
        nonce: entry.nonce,
        max_fee: fees.max_fee(),
        max_priority_fee: fees.max_priority_fee(),
        gas_limit: entry.gas_limit,
        to: entry.to,
        value: entry.value,
        data: entry.data.clone(),
    };
    let Ok(signature) = ecdsa::sign(tx.signing_hash()).await else {
        return false;
    };
    let signed = tx.into_signed(signature);
    let appended = append_event(EventType::TxReplaced {
        purpose: entry.purpose,
        chain_id,
        nonce: entry.nonce,
        max_fee: fees.max_fee(),
        max_priority_fee: fees.max_priority_fee(),
        tx_hash: signed.hash(),
        raw_tx: signed.raw().to_vec(),
    });
    if appended.is_err() {
        return false;
    }
    // A3: the entry this pass read is the one replaced, and only if it is still the one
    // the outbox holds
    if outbox::get(entry.key()).is_some_and(|current| current.tx_hash() == entry.tx_hash()) {
        outbox::put(entry.replaced(
            signed.hash(),
            signed.raw().to_vec(),
            fees.max_fee(),
            fees.max_priority_fee(),
        ));
    }
    true
}

/// The fees a replacement pays: double what the transaction being replaced paid, never
/// below what the chain is asking now, and never above what the freshest reading allows.
/// Nothing at all once the entry is already at that ceiling, or when the chain reading is
/// too old to price a bid against.
fn bumped(entry: &OutboxEntry, chain_id: ChainId, now: Timestamp) -> Option<Fees> {
    let reading = fresh_reading(chain_id, now).ok()?;
    let floor = reading.fees()?;
    let current = Fees::new(entry.max_fee, entry.max_priority_fee)?;
    let bumped = current.bumped(floor, reading.fee_ceiling())?;
    affordable(chain_id, bumped, entry.gas_limit).ok()?;
    Some(bumped)
}

/// What a receipt says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Receipt {
    tx_hash: TxHash,
    block: BlockNumber,
    success: bool,
}

/// Why a provider's answer is not a receipt this entry may be decided on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NotOurReceipt {
    /// The provider has not mined the transaction: it answered `null`, or a half-written
    /// record. Not a failure, just nothing to decide on yet.
    Unmined,
    /// A receipt for a transaction this entry never broadcast. A receipt decides whether an
    /// attempt confirmed or failed, and one unreplicated provider supplies both the receipt
    /// and the height it is measured against, so an answer about some other transaction
    /// must decide nothing at all.
    AnotherTransaction,
}

/// `0x`-prefixed hex as a block number.
fn parse_block_number(text: &str) -> Option<BlockNumber> {
    u64::from_str_radix(text.strip_prefix("0x")?, 16)
        .ok()
        .map(BlockNumber::new)
}

/// `0x`-prefixed hex as a thirty-two byte hash.
fn parse_hash32(value: Option<&Value>) -> Option<[u8; 32]> {
    let text = value?.as_str()?.strip_prefix("0x")?;
    <[u8; 32]>::try_from(hex::decode(text).ok()?).ok()
}

/// The receipt `value` carries, if it is a receipt about one of `ours`.
///
/// `ours` is every hash this entry has broadcast at its nonce, which is what binds the
/// answer to a transaction this canister actually signed. Without it the provider decides
/// on its own that an attempt is confirmed, for a transaction that need not exist, at a
/// height it also supplies. A receipt is also refused unless it names the block it is in:
/// a record with no block hash was mined into no block.
fn parse_receipt(value: &Value, ours: &[TxHash]) -> Result<Receipt, NotOurReceipt> {
    let read = || -> Option<Receipt> {
        let block = parse_block_number(value.get("blockNumber")?.as_str()?)?;
        parse_hash32(value.get("blockHash"))?;
        let tx_hash = TxHash::new(parse_hash32(value.get("transactionHash"))?);
        let status = value.get("status")?.as_str()?;
        Some(Receipt {
            tx_hash,
            block,
            success: status == "0x1",
        })
    };
    let receipt = read().ok_or(NotOurReceipt::Unmined)?;
    if !ours.contains(&receipt.tx_hash) {
        return Err(NotOurReceipt::AnotherTransaction);
    }
    Ok(receipt)
}

/// The entry a swap's open attempt is in, for the engine and for the tests.
pub fn open_entry(chain_id: ChainId, nonce: Nonce) -> Option<OutboxEntry> {
    outbox::get(NonceKey { chain_id, nonce })
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
            TxError::FeeOutOfRange { chain_id, ceiling } => Self::FeeOutOfRange {
                chain_id: chain_id.get(),
                ceiling_wei_per_gas: ceiling.into(),
            },
            TxError::GasCostTooHigh { chain_id, cost } => Self::GasCostTooHigh {
                chain_id: chain_id.get(),
                cost_wei: cost.into(),
                bound_wei: MAX_TRANSACTION_COST.into(),
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
