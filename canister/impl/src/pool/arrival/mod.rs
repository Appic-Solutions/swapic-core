//! `note_pool_arrival` and the canister's own read of Base (spec 3.1 step 4): the canister
//! reads the receipt itself, finds every USDC transfer to its own address in it, and proves
//! each against the rail that brought it, on logs it reads itself:
//!
//! - SameChain: the Runner's `PaidOut(id, canister, USDC, value)` and Swapic's
//!   `OrderStarted(id, ref, payer)` in the same receipt. A Runner transfer to the canister
//!   that no `PaidOut` explains is a fee step, which the books book, and is no arrival.
//! - CCTP: the USDC minted to the canister (a transfer from the zero address) and the
//!   `MessageReceived` of the pool chain's transmitter whose body mints it to the canister,
//!   burned by Swapic's Runner (`messageSender`) on a chain the canister knows (its domain
//!   and its `TokenMessengerV2`), the envelope in its hook data a Deliver naming the ref.
//!   The order's payer is on the source chain and not in this receipt, so it is not bound.
//! - Eco: the Portal's `IntentFulfilled(intentHash)` on Base, and on the source chain (one
//!   more read) the Portal's `IntentPublished(intentHash)` whose route is
//!   `USDC.transfer(canister, value)` into the pool chain, beside Swapic's one
//!   `OrderStarted(ref)`.
//!
//! No registration (P11d, Jeff 2026-10-21): the canister learns of a deposit only when its
//! USDC arrives. Whoever reports the arrival hands over the deposit's terms, the preimage of
//! the ref the order carries; the canister recomputes the ref and credits the transfer whose
//! proof carries it, checking the terms on what arrived: the parties not sanctioned, the
//! pool open, a swap within the per-swap cap with its ICP fees at least its path's own
//! ckUSDC fees and, on a ckUSDC payout, `arrived - premium - icp_fees` at least `min_out`;
//! a stake within the TVL cap. Terms it does not take hold the transfer for a controller,
//! with the terms and the payer. One deposit per ref: a second arrival under a credited ref
//! is held. A transfer whose proof fails for good is held with its payer when known. A
//! deposit's transfer found without its terms (the canister's own read of Base) waits 24
//! hours for them on the unattributed line, then is held for a controller. Every step of
//! one call is appended in the message that checks it, after the last read, so the same
//! transfer reported twice is credited once.

#[cfg(test)]
mod tests;

use crate::cctp::parse_address;
use crate::ecdsa;
use crate::pool;
use crate::rpc::{self, hex0x, parse_block_number, parse_hash32, RpcError};
use crate::state::{State, Store};
use crate::storage::events::read_state;
use crate::storage::{config, pool as pool_store};
use serde_json::{json, Value};
use settlement_api::types::pool::{PoolArrival, PoolArrivalOutcome, PoolError};
use std::time::Duration;
use types::hash::PoolRef;
use types::pool::{
    check_arrival, CreditKey, DepositKind, DepositStatus, DepositTerms, OrphanStatus, PoolAccount,
    PoolRail, PoolStep, Registered,
};
use types::swapic::{self, topics, Action, Envelope};
use types::{Address, BlockNumber, ChainId, EvmAddress, Timestamp, TokenAmount, TxHash};

/// How long a deposit's transfer found without its terms waits for them before it is a
/// controller's to resolve (P11d, Jeff 2026-10-21).
pub const AWAIT_TERMS: Duration = Duration::from_secs(24 * 60 * 60);

/// The terms an arrival's report hands over, the ref they commit to, and the source chain
/// the report names (proven by the Eco read; the pool chain on the other rails).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Given {
    pub terms: DepositTerms,
    pub r#ref: PoolRef,
    pub src_chain: ChainId,
}

/// What a report's terms are judged with beside the fold: why the pool is closed, if it is,
/// and the sanctions set.
pub struct Judge<'a> {
    pub given: Option<&'a Given>,
    pub closed: Option<String>,
    pub sanctioned: &'a dyn Fn(&Address) -> bool,
}

impl Judge<'_> {
    /// A judge with no terms: every deposit's transfer awaits its own.
    pub fn without_terms() -> Judge<'static> {
        Judge {
            given: None,
            closed: None,
            sanctioned: &|_| false,
        }
    }
}

/// The most one receipt read may answer.
const MAX_RECEIPT_BYTES: u64 = 512 * 1024;

/// A chain a CCTP arrival may leave from, as the canister's config names it: its CCTP domain,
/// its `TokenMessengerV2` (a burn's sender), and its Runner (the burn's `messageSender`: the
/// Runner makes an order's burn step).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CctpSource {
    pub domain: u32,
    pub token_messenger: EvmAddress,
    pub runner: EvmAddress,
}

/// The addresses a proof on the pool chain reads, all the canister's own configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PoolChain {
    pub chain: ChainId,
    pub usdc: EvmAddress,
    pub runner: EvmAddress,
    pub swapic: EvmAddress,
    pub transmitter: EvmAddress,
    pub eco_portal: Option<EvmAddress>,
    pub canister: EvmAddress,
    /// Every chain a CCTP arrival may come from.
    pub cctp_sources: Vec<CctpSource>,
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

    /// The log `value` is; its index read as `index` when it carries none.
    fn parse(value: &Value, index: Option<u64>) -> Option<Self> {
        Some(Log {
            index: hex_u64(value.get("logIndex")).or(index)?,
            address: parse_address(value.get("address")?.as_str()?)?,
            topics: value
                .get("topics")?
                .as_array()?
                .iter()
                .map(|topic| parse_hash32(Some(topic)))
                .collect::<Option<_>>()?,
            data: hex_bytes(value.get("data"))?,
            block_time: hex_u64(value.get("blockTimestamp")),
        })
    }
}

/// Every log of a receipt `value` that reads, in order, a removed one left out: what the
/// books' fee reading and the arrival's proof classify alike.
pub fn logs_of(value: &Value) -> Vec<Log> {
    value
        .get("logs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .filter(|(_, log)| log.get("removed").and_then(Value::as_bool) != Some(true))
        .filter_map(|(at, log)| Log::parse(log, u64::try_from(at).ok()))
        .collect()
}

/// A USDC transfer from the Runner to the canister, as one classifier reads it for both the
/// books and the pool: a same-chain order's payout to the canister, which a `PaidOut(id,
/// canister, USDC, value)` of the Runner in the same receipt explains (each payout explains
/// one transfer of its value), or else a relayed order's fee step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunnerTransfer {
    Payout {
        index: u64,
        id: [u8; 32],
        amount: TokenAmount,
    },
    Fee {
        index: u64,
        amount: TokenAmount,
    },
}

/// The Runner's USDC transfers to the canister in `logs`, each classified, in log order.
pub fn runner_transfers(
    logs: &[Log],
    usdc: EvmAddress,
    runner: EvmAddress,
    canister: EvmAddress,
) -> Vec<RunnerTransfer> {
    let mut payouts: Vec<([u8; 32], TokenAmount)> = logs
        .iter()
        .filter(|log| log.is(runner, topics::paid_out()))
        .filter(|log| log.topics.len() == 3 && log.topics[2] == canister.to_word())
        .filter(|log| log.data.len() == 64 && log.data[..32] == usdc.to_word())
        .map(|log| {
            let amount: [u8; 32] = log.data[32..].try_into().expect("BUG: 64 bytes");
            (log.topics[1], TokenAmount::from_be_bytes(amount))
        })
        .collect();
    logs.iter()
        .filter(|log| log.is(usdc, topics::transfer()) && log.topics.len() == 3)
        .filter(|log| log.topics[1] == runner.to_word() && log.topics[2] == canister.to_word())
        .filter_map(|log| {
            let value: [u8; 32] = log.data.as_slice().try_into().ok()?;
            Some((log.index, TokenAmount::from_be_bytes(value)))
        })
        .map(
            |(index, amount)| match payouts.iter().position(|(_, paid)| *paid == amount) {
                Some(at) => RunnerTransfer::Payout {
                    index,
                    id: payouts.remove(at).0,
                    amount,
                },
                None => RunnerTransfer::Fee { index, amount },
            },
        )
        .collect()
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
            logs.push(Log::parse(log, None)?);
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
            // a transfer of nothing proves nothing and is nothing to keep
            .filter(|(_, _, value)| *value > TokenAmount::ZERO)
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
    /// Not proven by what this report read (an Eco transfer the given source does not
    /// explain): left uncredited, for the report that brings its own source.
    Unproven { why: String },
    /// Eco's Portal refunded the canister, the creator, the reward of these unfilled intents
    /// in this transaction: a paid payout's whose intent is one of them, and whose amount this
    /// transfer is, takes it back.
    Refund { intents: Vec<[u8; 32]> },
}

/// The intents whose rewards Eco's Portal refunded to the canister in `base`.
fn refunded_intents(base: &Receipt, chain: &PoolChain) -> Vec<[u8; 32]> {
    let Some(portal) = chain.eco_portal else {
        return Vec::new();
    };
    base.logs
        .iter()
        .filter(|log| log.is(portal, topics::intent_refunded()))
        .filter_map(|log| swapic::read_intent_refunded(&log.topics, &log.data))
        .filter(|(_, refundee)| *refundee == chain.canister)
        .map(|(intent, _)| intent)
        .collect()
}

/// One transfer to the canister and what it proves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub credit: CreditKey,
    pub amount: TokenAmount,
    pub block: BlockNumber,
    /// When it arrived: its block's time where the provider gives it.
    pub at: Option<Timestamp>,
    /// The rail it came by: the Runner's same-chain payout, a CCTP mint, or else Eco.
    pub rail: PoolRail,
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
    let runner = runner_transfers(&base.logs, chain.usdc, chain.runner, chain.canister);
    let mut messages = received_messages(base, chain);
    let refunds = refunded_intents(base, chain);
    let mut found = Vec::new();
    for (log, from, amount) in transfers {
        let rail = if from == chain.runner {
            PoolRail::SameChain
        } else if from == EvmAddress::ZERO {
            PoolRail::Cctp
        } else {
            PoolRail::Eco
        };
        let proof = if from == chain.runner {
            let classified = runner.iter().find_map(|transfer| match transfer {
                RunnerTransfer::Payout { index, id, .. } if *index == log.index => Some(Some(*id)),
                RunnerTransfer::Fee { index, .. } if *index == log.index => Some(None),
                _ => None,
            });
            match classified {
                Some(Some(id)) => same_chain(base, chain, id),
                // the fee step of a relayed order: the books' line, never the pool's
                _ => continue,
            }
        } else if from == EvmAddress::ZERO {
            cctp(&mut messages, log.index, chain, amount)
        } else if !refunds.is_empty() {
            // an unfilled payout intent's reward, back to its creator
            Proof::Refund {
                intents: refunds.clone(),
            }
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
            rail,
            proof,
        });
    }
    if found.is_empty() {
        return Err(NoArrival::OnlyFees);
    }
    Ok(found)
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

/// The burn body's fields a CCTP proof reads.
struct BurnRead<'a> {
    mint_recipient: [u8; 32],
    /// The burn less Circle's fee: what was minted.
    minted: TokenAmount,
    /// Who burned on the source chain.
    message_sender: [u8; 32],
    hook: &'a [u8],
}

/// A version 1 burn body's mint recipient, minted amount, sender and hook data.
fn burn_body(body: &[u8]) -> Option<BurnRead<'_>> {
    let word = |at: usize| -> Option<[u8; 32]> { body.get(at..at + 32)?.try_into().ok() };
    if body.get(..4)? != 1_u32.to_be_bytes() {
        return None;
    }
    let amount = TokenAmount::from_be_bytes(word(68)?);
    let fee = TokenAmount::from_be_bytes(word(164)?);
    Some(BurnRead {
        mint_recipient: word(36)?,
        minted: amount.checked_sub(fee)?,
        message_sender: word(100)?,
        hook: body.get(228..)?,
    })
}

/// The messages the pool chain's transmitter received in `base`, each with its log index:
/// each mint to the canister takes its own, so two mints of one value in one transaction are
/// each matched to their own message.
fn received_messages(base: &Receipt, chain: &PoolChain) -> Vec<(u64, swapic::ReceivedMessage)> {
    base.logs
        .iter()
        .filter(|log| log.is(chain.transmitter, topics::message_received()))
        .filter_map(|log| {
            swapic::read_message_received(&log.topics, &log.data)
                .map(|message| (log.index, message))
        })
        .collect()
}

/// The CCTP proof of a mint to the canister at log `mint`: the first message received after
/// it (the transmitter logs a message after the mint it makes) that mints `amount` to the
/// canister, taken so no other mint reads it.
fn cctp(
    messages: &mut Vec<(u64, swapic::ReceivedMessage)>,
    mint: u64,
    chain: &PoolChain,
    amount: TokenAmount,
) -> Proof {
    let orphan = |why: &str| Proof::Orphan {
        payer: None,
        why: why.to_string(),
    };
    let position = messages.iter().position(|(index, message)| {
        *index > mint
            && burn_body(&message.body).is_some_and(|burn| {
                burn.mint_recipient == chain.canister.to_word() && burn.minted == amount
            })
    });
    let Some(position) = position else {
        return orphan("a mint to the canister that no received message explains");
    };
    let (_, message) = messages.remove(position);
    let burn = burn_body(&message.body).expect("BUG: the burn body was read to match it");
    // a burn Swapic's Runner made on a chain the canister knows, through that chain's
    // TokenMessengerV2: a burn anyone else made carries a ref it does not own
    let ours = chain.cctp_sources.iter().any(|source| {
        source.domain == message.source_domain
            && source.token_messenger.to_word() == message.sender
            && source.runner.to_word() == burn.message_sender
    });
    if !ours {
        return orphan("the burn was not made by Swapic's Runner on a chain the canister knows");
    }
    match Envelope::decode(burn.hook) {
        // the order's payer is on the source chain, which this receipt does not show: the
        // payer is not bound, so only a clean credit may take the ref
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
    // what the given source does not prove is left for the report that brings its own
    // source: never kept as an orphan under a payer borrowed from another transaction
    let unproven = |why: &str| {
        Ok(Proof::Unproven {
            why: why.to_string(),
        })
    };
    let orders = order_started(source, at.swapic, None);
    if !source.success {
        return unproven("the source transaction reverted");
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
        return unproven("no intent the source published delivers this transfer to the canister");
    }
    match orders.as_slice() {
        [(r#ref, payer)] => Ok(Proof::Deposit {
            r#ref: *r#ref,
            payer: *payer,
        }),
        _ => unproven("the source transaction holds no single order"),
    }
}

/// The payer, and the owner whoever it is: an EVM address as the set keys it, a principal
/// as its text.
fn screened(
    payer: Option<EvmAddress>,
    kind: &DepositKind,
    sanctioned: &dyn Fn(&Address) -> bool,
) -> Option<&'static str> {
    if payer.is_some_and(|payer| sanctioned(&Address::from(payer))) {
        return Some("the payer is sanctioned");
    }
    let owner = match kind.owner() {
        PoolAccount::Evm(address) => Some(Address::from(address)),
        PoolAccount::Icp(account) => account.owner.to_text().parse::<Address>().ok(),
    };
    owner
        .is_some_and(|owner| sanctioned(&owner))
        .then_some("the owner is sanctioned")
}

/// The terms in one line, kept with a transfer held for a controller.
fn summary(terms: &DepositTerms) -> String {
    let kind = match terms.kind {
        DepositKind::Swap {
            recipient,
            token_out,
            min_out,
        } => format!(
            "swap to {}{} of {token_out}, min_out {min_out}",
            recipient.owner,
            recipient
                .subaccount
                .map(|sub| format!(" subaccount {}", hex::encode(sub)))
                .unwrap_or_default()
        ),
        DepositKind::Stake { owner } => match owner {
            PoolAccount::Evm(address) => format!("stake for {address}"),
            PoolAccount::Icp(account) => format!("stake for {}", account.owner),
        },
    };
    format!(
        "{kind}, premium {}, icp_fees {}, rebalance_share {}, salt {}",
        terms.premium,
        terms.icp_fees,
        terms.rebalance_share,
        hex::encode(terms.salt)
    )
}

/// What a deposit's transfer whose terms came makes: its registration and its arrival line
/// (the registration skipped where one waits under the ref already), or why the pool does
/// not take it, with the terms.
fn decide<S: Store>(
    state: &State<S>,
    found: &Found,
    payer: Option<EvmAddress>,
    given: &Given,
    judge: &Judge<'_>,
    now: Timestamp,
) -> Result<Vec<PoolStep>, String> {
    let held = |why: &str| format!("{why}; held with its terms: {}", summary(&given.terms));
    // the source chain a report names is read on Eco only; the other rails are proven in
    // the pool chain's receipt
    let src_chain = match found.rail {
        PoolRail::Eco => given.src_chain,
        PoolRail::Cctp | PoolRail::SameChain => found.credit.chain,
    };
    let registration = given
        .terms
        .registration(found.amount, src_chain, payer, found.rail, now);
    // one deposit per ref; a registration left waiting under it (an arrival line refused
    // after it) is the one credited
    let waiting = match state.store().pool_deposit(&given.r#ref) {
        Some(deposit) if deposit.status == DepositStatus::Registered => Some(deposit),
        Some(deposit) => {
            return Err(held(&format!(
                "the ref was credited already ({:?})",
                deposit.status
            )))
        }
        None => None,
    };
    if let Some(reason) = &judge.closed {
        return Err(held(&format!("the pool is closed: {reason}")));
    }
    if let Some(why) = screened(payer, &given.terms.kind, judge.sanctioned) {
        return Err(held(why));
    }
    let ledger = state.pool();
    let registered = waiting.is_some();
    let deposit = waiting.unwrap_or_else(|| Registered::new(registration.clone()));
    check_arrival(&ledger, &deposit.terms).map_err(|refusal| held(&refusal.to_string()))?;
    if found.amount < deposit.terms.min_usdc {
        return Err(held(&format!(
            "{} arrived, below the {} registered",
            found.amount, deposit.terms.min_usdc
        )));
    }
    ledger
        .arrival_fits(&deposit, found.amount)
        .map_err(|why| held(&format!("{} arrived: {why}", found.amount)))?;
    let mut steps = Vec::with_capacity(2);
    if !registered {
        steps.push(PoolStep::Registered {
            r#ref: given.r#ref,
            deposit: registration,
        });
    }
    steps.push(PoolStep::Arrived {
        r#ref: given.r#ref,
        credit: found.credit,
        amount: found.amount,
        block: found.block,
    });
    Ok(steps)
}

/// What the fold makes of one transfer, judged at `now` (its block's time when known, for
/// its wait for terms): the steps to append, in order (none when it was credited already),
/// or why nothing is (a proof this report does not make, or terms that waited too long).
pub fn steps_for<S: Store>(
    state: &State<S>,
    found: &Found,
    judge: &Judge<'_>,
    now: Timestamp,
) -> Result<Vec<PoolStep>, String> {
    let credited = state.store().is_pool_credited(&found.credit);
    // a rebalance move's arrival, or an Eco refund of its intent (P11c): the move's, never a
    // deposit's, and never awaiting terms while its own transaction closes
    if !credited {
        if let Some(step) = crate::pool::rebalance::arrival::step_for(state, found) {
            return Ok(vec![step]);
        }
        if crate::pool::rebalance::arrival::waits_for_its_move(state, found) {
            return Err("a rebalance move's arrival waits for its move".to_string());
        }
    }
    let orphan = |payer: Option<EvmAddress>, why: String| PoolStep::Unattributed {
        credit: found.credit,
        amount: found.amount,
        src_payer: payer,
        why,
        block: found.block,
        r#ref: None,
        awaiting_until: None,
    };
    let (r#ref, payer) = match &found.proof {
        Proof::Unproven { why } => return Err(why.clone()),
        _ if credited
            && !state
                .store()
                .pool_orphan(&found.credit)
                .is_some_and(|orphan| {
                    matches!(orphan.status, OrphanStatus::AwaitingTerms { .. })
                }) =>
        {
            return Ok(Vec::new())
        }
        Proof::Orphan { payer, why } if !credited => return Ok(vec![orphan(*payer, why.clone())]),
        Proof::Orphan { .. } => return Ok(Vec::new()),
        Proof::Deposit { r#ref, payer } => (*r#ref, *payer),
        Proof::Refund { .. } if credited => return Ok(Vec::new()),
        Proof::Refund { intents } => {
            // the paid payout whose intent was refunded, for exactly its amount
            let paid = intents.iter().find_map(|intent| {
                let id = state.store().payout_by_intent(&PoolRef::new(*intent))?;
                let payout = state.store().pool_payout(id)?;
                (payout.status == types::pool::PayoutStatus::Paid
                    && payout.amount == Some(found.amount))
                .then_some(id)
            });
            return Ok(vec![match paid {
                Some(id) => PoolStep::PayoutRefunded {
                    id,
                    credit: found.credit,
                    amount: found.amount,
                    block: found.block,
                },
                None => orphan(
                    None,
                    "an Eco refund to the canister no paid payout of that amount takes".to_string(),
                ),
            }]);
        }
    };
    let Some(given) = judge.given.filter(|given| given.r#ref == r#ref) else {
        if credited {
            // it awaits its terms already
            return Ok(Vec::new());
        }
        let until = now
            .checked_add(AWAIT_TERMS)
            .unwrap_or(Timestamp::from_nanos(u64::MAX));
        return Ok(vec![PoolStep::Unattributed {
            credit: found.credit,
            amount: found.amount,
            src_payer: payer,
            why: format!("a deposit's transfer awaiting its terms (ref {ref})", ref = r#ref),
            block: found.block,
            r#ref: Some(r#ref),
            awaiting_until: Some(until),
        }]);
    };
    if credited {
        // a transfer awaiting its terms, which came: claimed while it may be
        let awaiting = state
            .store()
            .pool_orphan(&found.credit)
            .map(|orphan| orphan.status);
        if let Some(OrphanStatus::AwaitingTerms { until, .. }) = awaiting {
            if now > until {
                return Err(
                    "the transfer waited for its terms past its 24 hours: it is held for a controller"
                        .to_string(),
                );
            }
        }
        return Ok(match decide(state, found, payer, given, judge, now) {
            Ok(steps) => std::iter::once(PoolStep::TermsClaimed {
                credit: found.credit,
                refused: None,
            })
            .chain(steps)
            .collect(),
            Err(why) => vec![PoolStep::TermsClaimed {
                credit: found.credit,
                refused: Some(why),
            }],
        });
    }
    Ok(match decide(state, found, payer, given, judge, now) {
        Ok(steps) => steps,
        Err(why) => vec![orphan(payer, why)],
    })
}

/// What one report recorded: each transfer's outcome, and why the others were not recorded
/// (unproven by what was read, or refused by the fold). One transfer never stops the rest.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Recorded {
    pub outcomes: Vec<PoolArrivalOutcome>,
    /// The outcomes of the transfers whose proof carries the report's ref.
    pub mine: Vec<PoolArrivalOutcome>,
    /// Whether any transfer's proof carries the report's ref.
    pub carried: bool,
    pub not: Vec<String>,
}

/// The outcome a transfer's last appended step answers.
fn outcome_of(step: &PoolStep) -> Option<PoolArrivalOutcome> {
    match step {
        PoolStep::Arrived { r#ref, amount, .. } => Some(PoolArrivalOutcome::Credited {
            r#ref: r#ref.into_bytes().to_vec(),
            amount: candid::Nat::from(*amount),
        }),
        PoolStep::Unattributed { amount, why, .. } => Some(PoolArrivalOutcome::Unattributed {
            amount: candid::Nat::from(*amount),
            why: why.clone(),
        }),
        // an Eco refund credited back to its payout, named by the payout's ref
        PoolStep::PayoutRefunded { id, amount, .. } => Some(PoolArrivalOutcome::Credited {
            r#ref: types::pool::evm_payout_ref(*id).into_bytes().to_vec(),
            amount: candid::Nat::from(*amount),
        }),
        // a rebalance move's arrival or refund, named by the move's ref (P11c)
        PoolStep::MoveArrived {
            id,
            credited: amount,
            ..
        }
        | PoolStep::MoveRefunded { id, amount, .. } => Some(PoolArrivalOutcome::Credited {
            r#ref: types::pool::move_ref(*id).into_bytes().to_vec(),
            amount: candid::Nat::from(*amount),
        }),
        _ => None,
    }
}

/// Appends what `found` proves, in this message, each transfer on its own: the ones credited
/// already are left out, the unproven ones left for their own report, and a step the fold
/// refuses is noted and the rest of that transfer's steps left out.
pub fn record(found: &[Found], judge: &Judge<'_>, now: Timestamp) -> Recorded {
    let mut recorded = Recorded::default();
    for one in found {
        let mine = matches!(
            (&one.proof, judge.given),
            (Proof::Deposit { r#ref, .. }, Some(given)) if *r#ref == given.r#ref
        );
        recorded.carried |= mine;
        let steps = match read_state(|state| steps_for(state, one, judge, now)) {
            Ok(steps) => steps,
            Err(why) => {
                recorded.not.push(why);
                continue;
            }
        };
        let mut outcome = None;
        for step in steps {
            let refused_why = match &step {
                PoolStep::TermsClaimed {
                    refused: Some(why), ..
                } => Some(why.clone()),
                _ => None,
            };
            let answer = outcome_of(&step);
            match pool_store::record_at(step, now) {
                Ok(_) => {
                    outcome = answer.or(refused_why.map(|why| PoolArrivalOutcome::Unattributed {
                        amount: candid::Nat::from(one.amount),
                        why,
                    }));
                }
                Err(error) => {
                    pool::log(&format!(
                        "ALERT: the pool arrival {:?} was refused: {error}",
                        one.credit
                    ));
                    recorded.not.push(error.to_string());
                    outcome = None;
                    break;
                }
            }
        }
        if let Some(outcome) = outcome {
            if mine {
                recorded.mine.push(outcome.clone());
            }
            recorded.outcomes.push(outcome);
        }
    }
    recorded
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
    let cctp_sources = config
        .evm_chains
        .0
        .iter()
        .filter_map(|(chain, evm)| {
            Some(CctpSource {
                domain: config.cctp_domains.get(*chain)?.get(),
                token_messenger: evm.token_messenger,
                runner: evm.runner,
            })
        })
        .collect();
    Ok(PoolChain {
        chain,
        usdc: evm.usdc,
        runner: evm.runner,
        swapic: evm.swapic,
        transmitter: evm.message_transmitter,
        eco_portal: config.eco_portal,
        canister,
        cctp_sources,
    })
}

/// A read that answered nothing usable, as the caller is told: ask again.
fn unavailable(error: RpcError) -> PoolError {
    // a receipt too large for the read never grows smaller: asking again changes nothing
    if let RpcError::AnswerTooLarge { cap } = error {
        return PoolError::BadArrival(format!("the receipt is larger than the {cap} bytes read"));
    }
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

/// Reads `base_tx` on the pool chain (and `source` for an Eco arrival) and proves every
/// transfer to the canister in it, recording nothing: what a move's arrival reads (P11c).
pub async fn read_found(
    base_tx: TxHash,
    source: Option<(ChainId, TxHash)>,
) -> Result<Vec<Found>, PoolError> {
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
    find(
        &base,
        source.as_ref().map(|(receipt, at)| (receipt, at)),
        &chain,
    )
    .map_err(|no| PoolError::BadArrival(no.why().to_string()))
}

/// Reads `base_tx` on the pool chain (and `source` for an Eco arrival), proves every
/// transfer to the canister in it, and records them on `given`'s terms.
pub async fn read_and_record(
    base_tx: TxHash,
    source: Option<(ChainId, TxHash)>,
    given: Option<&Given>,
) -> Result<Recorded, PoolError> {
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
    // after the last read: the credited set, the pool's guard and the append are in this
    // one message
    pool::require_running()?;
    let judge = Judge {
        given,
        closed: pool::pool_closed(),
        sanctioned: &crate::storage::sanctions::is_sanctioned,
    };
    Ok(record(&found, &judge, pool::now()))
}

/// Anyone's door (P11d: the watcher, or the user's app as a backup): the body of
/// `note_pool_arrival`. The canister trusts only the chain and the fingerprint: the terms
/// credit only the transfer whose proof carries the ref they commit to.
pub async fn note(arrival: PoolArrival) -> Result<PoolArrivalOutcome, PoolError> {
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
    let terms = arrival.terms.into_domain().map_err(PoolError::from)?;
    // a fee the ref does not commit would let anyone hand the user's ref other terms
    if !terms.is_canonical() {
        return Err(PoolError::BadArrival(
            "the terms carry a fee their ref does not commit".to_string(),
        ));
    }
    let r#ref = terms
        .pool_ref()
        .ok_or_else(|| PoolError::BadArrival("the terms' fees fit no ref".to_string()))?;
    let given = Given {
        terms,
        r#ref,
        src_chain: ChainId::new(arrival.src_chain),
    };
    let recorded = read_and_record(base_tx, source, Some(&given)).await?;
    let credited = recorded
        .mine
        .iter()
        .find(|outcome| matches!(outcome, PoolArrivalOutcome::Credited { .. }));
    if let Some(outcome) = credited.or(recorded.mine.first()) {
        return Ok(outcome.clone());
    }
    if !recorded.carried {
        return Err(PoolError::BadArrival(
            recorded
                .not
                .first()
                .cloned()
                .unwrap_or_else(|| "no transfer in the receipt carries the terms' ref".to_string()),
        ));
    }
    match recorded.not.first() {
        Some(why) => Err(PoolError::BadArrival(why.clone())),
        None => Err(PoolError::AlreadyCredited),
    }
}
