//! `note_pool_arrival` and the canister's own read of Base (spec 3.1 step 4): the canister
//! reads the receipt itself, finds every USDC transfer to its own address in it, and proves
//! each against the rail that brought it, on logs it reads itself:
//!
//! - SameChain: the Runner's `PaidOut(id, canister, USDC, value)` and Swapic's
//!   `OrderStarted(id, ref, payer)` in the same receipt. A Runner transfer to the canister
//!   that no `PaidOut` explains is a fee step, which the books book, and is no arrival.
//! - CCTP: the USDC minted to the canister (a transfer from the zero address) and the
//!   `MessageReceived` of the pool chain's transmitter whose body mints it to the canister,
//!   the envelope in its hook data a Deliver naming the ref.
//! - Eco: the Portal's `IntentFulfilled(intentHash)` on Base, and on the source chain (one
//!   more read) the Portal's `IntentPublished(intentHash)` whose route is
//!   `USDC.transfer(canister, value)` into the pool chain, beside Swapic's one
//!   `OrderStarted(ref)`.
//!
//! A transfer whose proof holds names a ref; one whose proof fails for good is recorded
//! unattributed with its payer when known, so the orphan refund (P11b) returns it. Then
//! against the fold: registered, still `Registered`, arrived by its expiry and grace (at its
//! block's time), at least `min_usdc`, within the pool's caps on what arrived, paid by the
//! registered payer, never credited. Every
//! step of one call is appended in the message that checks it, after the last read, so the
//! same transfer reported twice is credited once.

#[cfg(test)]
mod tests;

use crate::cctp::parse_address;
use crate::ecdsa;
use crate::guards::require_watcher;
use crate::pool;
use crate::rpc::{self, hex0x, parse_block_number, parse_hash32, RpcError};
use crate::state::{State, Store};
use crate::storage::events::read_state;
use crate::storage::{config, pool as pool_store};
use serde_json::{json, Value};
use settlement_api::types::pool::{PoolArrival, PoolArrivalOutcome, PoolError};
use types::hash::PoolRef;
use types::pool::{CreditKey, DepositStatus, PoolStep};
use types::swapic::{self, topics, Action, Envelope};
use types::{BlockNumber, ChainId, EvmAddress, Timestamp, TokenAmount, TxHash};

/// The most one receipt read may answer.
const MAX_RECEIPT_BYTES: u64 = 512 * 1024;

/// The addresses a proof on the pool chain reads, all the canister's own configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoolChain {
    pub chain: ChainId,
    pub usdc: EvmAddress,
    pub runner: EvmAddress,
    pub swapic: EvmAddress,
    pub transmitter: EvmAddress,
    pub eco_portal: Option<EvmAddress>,
    pub canister: EvmAddress,
}

/// The addresses an Eco proof reads on the order's own chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceChain {
    pub chain: ChainId,
    pub swapic: EvmAddress,
    pub eco_portal: Option<EvmAddress>,
}

/// One log as a receipt carries it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Log {
    pub index: u64,
    pub address: EvmAddress,
    pub topics: Vec<[u8; 32]>,
    pub data: Vec<u8>,
    /// The block's time, in seconds, where the provider gives it.
    pub block_time: Option<u64>,
}

impl Log {
    fn is(&self, emitter: EvmAddress, topic: [u8; 32]) -> bool {
        self.address == emitter && self.topics.first() == Some(&topic)
    }
}

/// A receipt, as far as a proof reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Receipt {
    pub tx: TxHash,
    pub block: BlockNumber,
    pub success: bool,
    pub logs: Vec<Log>,
}

fn hex_u64(value: Option<&Value>) -> Option<u64> {
    u64::from_str_radix(value?.as_str()?.strip_prefix("0x")?, 16).ok()
}

fn hex_bytes(value: Option<&Value>) -> Option<Vec<u8>> {
    hex::decode(value?.as_str()?.strip_prefix("0x")?).ok()
}

impl Receipt {
    /// The receipt `value` is, if it is one: its hash, block and status, and its logs, any
    /// a reorg removed left out.
    pub fn parse(value: &Value) -> Option<Self> {
        let tx = TxHash::new(parse_hash32(value.get("transactionHash"))?);
        let block = value
            .get("blockNumber")
            .and_then(Value::as_str)
            .and_then(parse_block_number)?;
        let success = value.get("status").and_then(Value::as_str) == Some("0x1");
        let mut logs = Vec::new();
        for log in value.get("logs")?.as_array()? {
            if log.get("removed").and_then(Value::as_bool) == Some(true) {
                continue;
            }
            logs.push(Log {
                index: hex_u64(log.get("logIndex"))?,
                address: parse_address(log.get("address")?.as_str()?)?,
                topics: log
                    .get("topics")?
                    .as_array()?
                    .iter()
                    .map(|topic| parse_hash32(Some(topic)))
                    .collect::<Option<_>>()?,
                data: hex_bytes(log.get("data"))?,
                block_time: hex_u64(log.get("blockTimestamp")),
            });
        }
        Some(Self {
            tx,
            block,
            success,
            logs,
        })
    }

    /// Every USDC transfer to the canister: its log, sender and value.
    fn transfers_to(&self, chain: &PoolChain) -> Vec<(&Log, EvmAddress, TokenAmount)> {
        self.logs
            .iter()
            .filter(|log| log.is(chain.usdc, topics::transfer()))
            .filter(|log| log.topics.len() == 3 && log.topics[2] == chain.canister.to_word())
            .filter_map(|log| {
                let from = swapic::evm_of_word(log.topics[1])?;
                let value: [u8; 32] = log.data.as_slice().try_into().ok()?;
                Some((log, from, TokenAmount::from_be_bytes(value)))
            })
            .collect()
    }
}

/// What a transfer to the canister proves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Proof {
    /// A deposit under `r#ref`; `payer` the order's, where the chain shows it.
    Deposit {
        r#ref: PoolRef,
        payer: Option<EvmAddress>,
    },
    /// No deposit, for good: kept unattributed.
    Orphan {
        payer: Option<EvmAddress>,
        why: String,
    },
}

/// One transfer to the canister and what it proves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub credit: CreditKey,
    pub amount: TokenAmount,
    pub block: BlockNumber,
    /// When it arrived: its block's time where the provider gives it.
    pub at: Option<Timestamp>,
    pub proof: Proof,
}

/// Why a receipt holds no arrival to record. Nothing is written for any of these.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NoArrival {
    Reverted,
    NoTransfer,
    /// Every transfer to the canister is a fee step.
    OnlyFees,
    /// An Eco arrival is proven on its source transaction, and none was read.
    NeedsSource,
}

impl NoArrival {
    pub fn why(&self) -> &'static str {
        match self {
            NoArrival::Reverted => "the transaction reverted",
            NoArrival::NoTransfer => "the receipt moves no USDC to the canister",
            NoArrival::OnlyFees => "every transfer to the canister is a fee step",
            NoArrival::NeedsSource => "an Eco arrival needs its source transaction",
        }
    }
}

/// Whether the receipt holds a transfer only an Eco proof can explain, which needs the
/// source transaction read.
pub fn needs_source(base: &Receipt, chain: &PoolChain) -> bool {
    let Some(portal) = chain.eco_portal else {
        return false;
    };
    let fulfilled = base
        .logs
        .iter()
        .any(|log| log.is(portal, topics::intent_fulfilled()));
    fulfilled
        && base
            .transfers_to(chain)
            .iter()
            .any(|(_, from, _)| *from != chain.runner && *from != EvmAddress::ZERO)
}

/// Every transfer to the canister in `base` and what it proves, reading `source` for an Eco
/// arrival.
pub fn find(
    base: &Receipt,
    source: Option<(&Receipt, &SourceChain)>,
    chain: &PoolChain,
) -> Result<Vec<Found>, NoArrival> {
    if !base.success {
        return Err(NoArrival::Reverted);
    }
    let transfers = base.transfers_to(chain);
    if transfers.is_empty() {
        return Err(NoArrival::NoTransfer);
    }
    let mut payouts = paid_outs(base, chain);
    let mut found = Vec::new();
    for (log, from, amount) in transfers {
        let proof = if from == chain.runner {
            match payouts.iter().position(|(_, paid)| *paid == amount) {
                // a payout is consumed by the transfer it explains, so two transfers of one
                // value need two payouts
                Some(at) => same_chain(base, chain, payouts.remove(at).0),
                // the fee step of a relayed order: the books' line, never the pool's
                None => continue,
            }
        } else if from == EvmAddress::ZERO {
            cctp(base, chain, amount)
        } else {
            eco(base, source, chain, amount)?
        };
        found.push(Found {
            credit: CreditKey {
                chain: chain.chain,
                tx: base.tx,
                log_index: log.index,
            },
            amount,
            block: base.block,
            at: log.block_time.and_then(Timestamp::from_secs),
            proof,
        });
    }
    if found.is_empty() {
        return Err(NoArrival::OnlyFees);
    }
    Ok(found)
}

/// The Runner's payouts to the canister in USDC: each order's id and amount.
fn paid_outs(receipt: &Receipt, chain: &PoolChain) -> Vec<([u8; 32], TokenAmount)> {
    receipt
        .logs
        .iter()
        .filter(|log| log.is(chain.runner, topics::paid_out()))
        .filter(|log| log.topics.len() == 3 && log.topics[2] == chain.canister.to_word())
        .filter(|log| log.data.len() == 64 && log.data[..32] == chain.usdc.to_word())
        .map(|log| {
            let amount: [u8; 32] = log.data[32..].try_into().expect("BUG: 64 bytes");
            (log.topics[1], TokenAmount::from_be_bytes(amount))
        })
        .collect()
}

/// The ref and payer of the order `id` names, from Swapic's `OrderStarted` in `receipt`.
fn order_started(
    receipt: &Receipt,
    swapic: EvmAddress,
    id: Option<[u8; 32]>,
) -> Vec<(PoolRef, Option<EvmAddress>)> {
    receipt
        .logs
        .iter()
        .filter(|log| log.is(swapic, topics::order_started()) && log.topics.len() == 4)
        .filter(|log| id.is_none_or(|id| log.topics[1] == id))
        .map(|log| {
            (
                PoolRef::new(log.topics[2]),
                swapic::evm_of_word(log.topics[3]),
            )
        })
        .collect()
}

fn same_chain(base: &Receipt, chain: &PoolChain, id: [u8; 32]) -> Proof {
    match order_started(base, chain.swapic, Some(id)).as_slice() {
        [(r#ref, payer)] => Proof::Deposit {
            r#ref: *r#ref,
            payer: *payer,
        },
        _ => Proof::Orphan {
            payer: None,
            why: "a payout to the canister with no order started for it".to_string(),
        },
    }
}

/// The burn body's fields a CCTP proof reads: version 1, its mint recipient, the amount
/// minted (the burn less Circle's fee), and its hook data.
fn burn_body(body: &[u8]) -> Option<([u8; 32], TokenAmount, &[u8])> {
    let word = |at: usize| -> Option<[u8; 32]> { body.get(at..at + 32)?.try_into().ok() };
    if body.get(..4)? != 1_u32.to_be_bytes() {
        return None;
    }
    let mint_recipient = word(36)?;
    let amount = TokenAmount::from_be_bytes(word(68)?);
    let fee = TokenAmount::from_be_bytes(word(164)?);
    Some((mint_recipient, amount.checked_sub(fee)?, body.get(228..)?))
}

fn cctp(base: &Receipt, chain: &PoolChain, amount: TokenAmount) -> Proof {
    let orphan = |why: &str| Proof::Orphan {
        payer: None,
        why: why.to_string(),
    };
    let bodies: Vec<Vec<u8>> = base
        .logs
        .iter()
        .filter(|log| log.is(chain.transmitter, topics::message_received()))
        .filter_map(|log| swapic::read_message_received(&log.topics, &log.data))
        .collect();
    let mut minted_here = bodies.iter().filter_map(|body| {
        let (recipient, minted, hook) = burn_body(body)?;
        (recipient == chain.canister.to_word() && minted == amount).then_some(hook)
    });
    let Some(hook) = minted_here.next() else {
        return orphan("a mint to the canister that no received message explains");
    };
    match Envelope::decode(hook) {
        Ok(envelope) if envelope.action == Action::Deliver => Proof::Deposit {
            r#ref: PoolRef::new(envelope.reference),
            payer: None,
        },
        Ok(_) => orphan("the message's envelope is not a Deliver"),
        Err(_) => orphan("the message carries no envelope of ours"),
    }
}

fn eco(
    base: &Receipt,
    source: Option<(&Receipt, &SourceChain)>,
    chain: &PoolChain,
    amount: TokenAmount,
) -> Result<Proof, NoArrival> {
    let orphan = |payer: Option<EvmAddress>, why: &str| {
        Ok(Proof::Orphan {
            payer,
            why: why.to_string(),
        })
    };
    let fulfilled: Vec<[u8; 32]> = chain
        .eco_portal
        .map(|portal| {
            base.logs
                .iter()
                .filter(|log| log.is(portal, topics::intent_fulfilled()) && log.topics.len() >= 2)
                .map(|log| log.topics[1])
                .collect()
        })
        .unwrap_or_default();
    if fulfilled.is_empty() {
        return orphan(None, "a transfer to the canister that proves no deposit");
    }
    let Some((source, at)) = source else {
        return Err(NoArrival::NeedsSource);
    };
    let Some(portal) = at.eco_portal else {
        return Err(NoArrival::NeedsSource);
    };
    let orders = order_started(source, at.swapic, None);
    let payer = match orders.as_slice() {
        [(_, payer)] => *payer,
        _ => None,
    };
    if !source.success {
        return orphan(payer, "the source transaction reverted");
    }
    let delivers_here = source
        .logs
        .iter()
        .filter(|log| log.is(portal, topics::intent_published()))
        .filter_map(|log| swapic::read_intent_published(&log.topics, &log.data))
        .filter(|intent| fulfilled.contains(&intent.intent_hash))
        .filter(|intent| intent.destination == chain.chain.get())
        .any(|intent| {
            swapic::route_delivery(&intent.route).is_some_and(|(token, to, value)| {
                token == chain.usdc
                    && to == chain.canister
                    && TokenAmount::from_be_bytes(value.to_be_bytes()) == amount
            })
        });
    if !delivers_here {
        return orphan(
            payer,
            "no intent the source published delivers this transfer to the canister",
        );
    }
    match orders.as_slice() {
        [(r#ref, payer)] => Ok(Proof::Deposit {
            r#ref: *r#ref,
            payer: *payer,
        }),
        _ => orphan(None, "the source transaction holds no single order"),
    }
}

/// What the fold makes of one transfer, judged at `now` (its block's time when known):
/// `Arrived`, or `Unattributed` with why and the payer to refund. None when the transfer was
/// credited already.
pub fn step_for<S: Store>(state: &State<S>, found: &Found, now: Timestamp) -> Option<PoolStep> {
    if state.store().is_pool_credited(&found.credit) {
        return None;
    }
    let at = found.at.map_or(now, |at| at.min(now));
    let orphan =
        |payer: Option<EvmAddress>, why: String, r#ref: Option<PoolRef>| PoolStep::Unattributed {
            credit: found.credit,
            amount: found.amount,
            src_payer: payer,
            why,
            block: found.block,
            r#ref,
        };
    let (r#ref, payer) = match &found.proof {
        Proof::Orphan { payer, why } => return Some(orphan(*payer, why.clone(), None)),
        Proof::Deposit { r#ref, payer } => (*r#ref, *payer),
    };
    let Some(deposit) = state.store().pool_deposit(&r#ref) else {
        return Some(orphan(
            payer,
            "no deposit is registered under the ref".to_string(),
            None,
        ));
    };
    let terms = &deposit.terms;
    if deposit.status != DepositStatus::Registered {
        return Some(orphan(
            payer.or(Some(terms.src_payer)),
            format!("the deposit is {:?}", deposit.status),
            Some(r#ref),
        ));
    }
    if let Some(paid_by) = payer.filter(|paid_by| *paid_by != terms.src_payer) {
        // another payer's money under this ref: theirs to get back, and the registration
        // stays the user's
        return Some(orphan(
            Some(paid_by),
            "paid by another than the registered payer".to_string(),
            None,
        ));
    }
    let grace = state
        .pool()
        .settings
        .map(|settings| settings.deposit_grace)
        .unwrap_or_default();
    if at > terms.credit_until(grace) {
        return Some(orphan(
            Some(terms.src_payer),
            "arrived past its expiry and grace".to_string(),
            Some(r#ref),
        ));
    }
    if found.amount < terms.min_usdc {
        return Some(orphan(
            Some(terms.src_payer),
            format!(
                "{} arrived, below the {} registered",
                found.amount, terms.min_usdc
            ),
            Some(r#ref),
        ));
    }
    // the caps, on what arrived rather than what was registered
    if let Err(why) = state.pool().arrival_fits(&deposit, found.amount) {
        return Some(orphan(
            Some(terms.src_payer),
            format!("{} arrived: {why}", found.amount),
            Some(r#ref),
        ));
    }
    Some(PoolStep::Arrived {
        r#ref,
        credit: found.credit,
        amount: found.amount,
        block: found.block,
    })
}

/// Appends what `found` proves, in this message, and answers each transfer's outcome; the
/// ones credited already are left out.
pub fn record(found: &[Found], now: Timestamp) -> Result<Vec<PoolArrivalOutcome>, PoolError> {
    let mut outcomes = Vec::new();
    for one in found {
        let Some(step) = read_state(|state| step_for(state, one, now)) else {
            continue;
        };
        let outcome = match &step {
            PoolStep::Arrived { r#ref, amount, .. } => PoolArrivalOutcome::Credited {
                r#ref: r#ref.into_bytes().to_vec(),
                amount: candid::Nat::from(*amount),
            },
            PoolStep::Unattributed { amount, why, .. } => PoolArrivalOutcome::Unattributed {
                amount: candid::Nat::from(*amount),
                why: why.clone(),
            },
            _ => unreachable!("BUG: step_for answers an arrival or an orphan"),
        };
        pool_store::record_at(step, now)
            .map_err(|error| PoolError::BadArrival(error.to_string()))?;
        outcomes.push(outcome);
    }
    Ok(outcomes)
}

/// The pool chain's addresses, from the config and the canister's own address.
pub async fn pool_chain() -> Result<PoolChain, PoolError> {
    let config = config::get();
    let chain = config
        .pool_chain
        .ok_or_else(|| PoolError::Closed("no pool chain is configured".to_string()))?;
    let evm = config
        .evm_chains
        .get(chain)
        .ok_or_else(|| PoolError::Closed("the pool chain has no Swapic configured".to_string()))?;
    let canister = ecdsa::canister_address()
        .await
        .map_err(|_| PoolError::Unavailable("the canister's address".to_string()))?;
    Ok(PoolChain {
        chain,
        usdc: evm.usdc,
        runner: evm.runner,
        swapic: evm.swapic,
        transmitter: evm.message_transmitter,
        eco_portal: config.eco_portal,
        canister,
    })
}

/// A read that answered nothing usable, as the caller is told: ask again.
fn unavailable(error: RpcError) -> PoolError {
    // the kind only: a provider's text can carry its url and key
    PoolError::Unavailable(format!("the read failed: {:?}", error.kind()))
}

/// The pool chain's receipt of `tx` and its head, in one outcall, the receipt at the
/// crediting depth.
pub async fn read_base(chain: &PoolChain, tx: TxHash) -> Result<Receipt, PoolError> {
    let calls = [
        ("eth_getTransactionReceipt", json!([hex0x(tx.as_ref())])),
        ("eth_blockNumber", json!([])),
    ];
    let answers = rpc::rpc_batch_each(chain.chain, &calls, MAX_RECEIPT_BYTES)
        .await
        .map_err(unavailable)?;
    let mut answers = answers.into_iter();
    let receipt = answers
        .next()
        .ok_or_else(|| PoolError::Unavailable("no receipt answer".to_string()))?
        .map_err(unavailable)?;
    let head = answers
        .next()
        .ok_or_else(|| PoolError::Unavailable("no head answer".to_string()))?
        .map_err(unavailable)?;
    if receipt.is_null() {
        return Err(PoolError::Unavailable(
            "the transaction has no receipt yet".to_string(),
        ));
    }
    let receipt = Receipt::parse(&receipt)
        .filter(|receipt| receipt.tx == tx)
        .ok_or_else(|| PoolError::BadArrival("the receipt does not read".to_string()))?;
    let head = head
        .as_str()
        .and_then(parse_block_number)
        .ok_or_else(|| PoolError::Unavailable("the head does not read".to_string()))?;
    deep_enough(receipt.block, head, chain.chain)?;
    Ok(receipt)
}

/// The receipt at `block` is at the pool chain's crediting depth under `head`.
pub fn deep_enough(block: BlockNumber, head: BlockNumber, chain: ChainId) -> Result<(), PoolError> {
    let needed = crate::tx::credit_confirmations(&config::get(), chain)
        .map_err(|_| PoolError::Closed("the pool chain has no depth configured".to_string()))?;
    if types::tx::is_confirmed(block, head, needed) {
        return Ok(());
    }
    let have = head
        .get()
        .checked_sub(block.get())
        .map_or(0, |behind| behind.saturating_add(1));
    Err(PoolError::NotDeep {
        needed: needed.get().max(1),
        have,
    })
}

/// The source chain's receipt of `tx`, in one outcall.
async fn read_source(chain: ChainId, tx: TxHash) -> Result<Receipt, PoolError> {
    let calls = [("eth_getTransactionReceipt", json!([hex0x(tx.as_ref())]))];
    let mut answers = rpc::rpc_batch_each(chain, &calls, MAX_RECEIPT_BYTES)
        .await
        .map_err(unavailable)?;
    let receipt = answers
        .pop()
        .ok_or_else(|| PoolError::Unavailable("no receipt answer".to_string()))?
        .map_err(unavailable)?;
    if receipt.is_null() {
        return Err(PoolError::Unavailable(
            "the source transaction has no receipt".to_string(),
        ));
    }
    Receipt::parse(&receipt)
        .filter(|receipt| receipt.tx == tx)
        .ok_or_else(|| PoolError::BadArrival("the source receipt does not read".to_string()))
}

/// Reads `base_tx` on the pool chain (and `source` for an Eco arrival), proves every
/// transfer to the canister in it, and records them.
pub async fn read_and_record(
    base_tx: TxHash,
    source: Option<(ChainId, TxHash)>,
) -> Result<Vec<PoolArrivalOutcome>, PoolError> {
    let chain = pool_chain().await?;
    let base = read_base(&chain, base_tx).await?;
    let source = if needs_source(&base, &chain) {
        let (src_chain, src_tx) = source
            .ok_or_else(|| PoolError::BadArrival(NoArrival::NeedsSource.why().to_string()))?;
        let swapic = config::get()
            .evm_chains
            .get(src_chain)
            .ok_or_else(|| {
                PoolError::BadArrival("the source chain is not one Swapic runs on".to_string())
            })?
            .swapic;
        let at = SourceChain {
            chain: src_chain,
            swapic,
            eco_portal: config::get().eco_portal,
        };
        Some((read_source(src_chain, src_tx).await?, at))
    } else {
        None
    };
    let found = find(
        &base,
        source.as_ref().map(|(receipt, at)| (receipt, at)),
        &chain,
    )
    .map_err(|no| PoolError::BadArrival(no.why().to_string()))?;
    // after the last read: the credited set and the append are in this one message
    pool::require_running()?;
    record(&found, pool::now())
}

/// Watcher-only door: the body of `note_pool_arrival`.
pub async fn note(arrival: PoolArrival) -> Result<PoolArrivalOutcome, PoolError> {
    require_watcher().map_err(pool::guard_error)?;
    pool::require_running()?;
    let base_tx = TxHash::new(
        settlement_api::types::pool::bytes32("base_tx", &arrival.base_tx)
            .map_err(PoolError::from)?,
    );
    let source = if arrival.src_tx.is_empty() {
        None
    } else {
        let tx = settlement_api::types::pool::bytes32("src_tx", &arrival.src_tx)
            .map_err(PoolError::from)?;
        Some((ChainId::new(arrival.src_chain), TxHash::new(tx)))
    };
    let outcomes = read_and_record(base_tx, source).await?;
    let credited = outcomes
        .iter()
        .find(|outcome| matches!(outcome, PoolArrivalOutcome::Credited { .. }));
    credited
        .or(outcomes.first())
        .cloned()
        .ok_or(PoolError::AlreadyCredited)
}
