//! The send path, and the rules A4 to A6 that make it safe.
//!
//! One transaction is created, signed and sent in that order, and the order is the whole
//! point:
//!
//! - `TxCreated` allocates the nonce and is appended with NO await before it (A1), so two
//!   calls that interleave at the signature cannot come back to the same number (A4).
//! - The signature is asked for after the allocation, and the signed record holds the exact
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

mod bump;
use bump::*;

use crate::ecdsa::{self, EcdsaError};
use crate::guards::require_not_halted;
use crate::rpc::{self, hex0x, parse_block_number, parse_hash32, RpcError, MAX_BLOCK_NUMBER_BYTES};
use crate::state::{State, Store};
use crate::storage::events::{append_event, read_state, AppendError};
use crate::storage::{chain_data, config, outbox, timer_state};
use crate::task_manager;
use serde_json::{json, Value};
use settlement_api::types::errors::GuardError;
use std::collections::BTreeMap;
use std::time::Duration;
use thiserror::Error;
use types::chain_data::{ChainData, Fees, MAX_FEE_PER_GAS};
use types::config::{GasAction, MIN_ETHEREUM_CONFIRMATIONS};
use types::events::TxPurpose;
use types::tx::{blocks_to_wait, final_through, is_final, next_receipt_read, MIN_RECEIPT_PACE};
use types::{
    BlockDepth, BlockNumber, ChainId, Eip1559Tx, EventIndex, EventType, EvmAddress, GasAmount,
    Nonce, NonceKey, OutboxEntry, OutboxStatus, QuoteHash, Timestamp, TxHash, Wei, WeiPerGas,
};

/// How long the current bytes stay out before they are handed to a provider again. A
/// rebroadcast costs one call and fixes the common case, which is a provider that dropped
/// the transaction from its mempool.
const REBROADCAST_AFTER: Duration = Duration::from_secs(30);

/// How long a transaction stays on the network before it is replaced at a higher fee,
/// counted from the FIRST time its bytes went out. Long enough that a chain running
/// normally lands it first, short enough that a swap is not held by an underpriced
/// transaction. 30 s: a transaction not in a block within 30 s is
/// replaced at a higher fee; the per-action budgets and the bounds cap the bids.
const STUCK_AFTER: Duration = Duration::from_secs(30);

/// How long an allocated nonce may wait for its signed record before the pass spends it
/// with a cancel: five minutes.
///
/// This clock is its own and is never the batch window. The signature is asked for after
/// `TxCreated`, and that await does not run to its end inside one message on this subnet:
/// it crosses to the signing subnet and spans the consensus rounds a threshold signature
/// takes there, measured at 2.7 seconds for `key_1` on mainnet, against a
/// default window of two seconds. A cancel fired inside that round trip races the signature
/// it was meant to stand in for, and the fold refuses whichever comes second
/// (`NonceNotUnsigned`), so nothing breaks, but a cancel that was never needed is signed
/// and broadcast. Five minutes is far above any round trip the signing subnet takes under
/// load, and short enough that a number nothing carries holds up the chain's queue for
/// minutes rather than hours. A config knob is a later plan.
const STRANDED_AFTER: Duration = Duration::from_secs(300);

/// The most any one transaction this canister signs may spend on gas: one ether, which is
/// two orders of magnitude above what a settlement transaction on the most congested chain
/// this canister sends to costs, and far below what a broken fee reading would produce.
///
/// The per-gas ceiling and the multiple a replacement may bid are the other two bounds; this
/// one is the product of a price and a limit, so it catches a gas limit nobody bounded as
/// well as a price. Like the other two it is a constant rather than a config knob, so no
/// config write can raise it; the knob belongs with the engine's gas policy in a later plan.
const MAX_TRANSACTION_COST: Wei = Wei::new(1_000_000_000_000_000_000);

/// Polygon pays gas in POL, worth a few thousandths of ETH, so 1 POL (about $0.09) refused
/// a CCTP completion once the base fee passed about 400 gwei. 50 POL is about $5.
const POLYGON_MAX_TRANSACTION_COST: Wei = Wei::new(50_000_000_000_000_000_000);

/// The most this canister pays per unit of gas on `chain_id`. The ETH-priced chains keep
/// 1,000 gwei, so a broken or hostile reading there is refused as before; Polygon, priced
/// in POL, gets the type-level ceiling [`MAX_FEE_PER_GAS`] (50,000 gwei) for its busy hours.
fn max_fee_per_gas(chain_id: ChainId) -> WeiPerGas {
    if chain_id == ChainId::POLYGON {
        MAX_FEE_PER_GAS
    } else {
        WeiPerGas::new(1_000_000_000_000)
    }
}

/// The bound of one transaction's gas bill on `chain_id`, in its native coin. Per chain,
/// because a native coin's worth differs by orders of magnitude; still a constant.
fn max_transaction_cost(chain_id: ChainId) -> Wei {
    if chain_id == ChainId::POLYGON {
        POLYGON_MAX_TRANSACTION_COST
    } else {
        MAX_TRANSACTION_COST
    }
}

/// The most one `eth_getTransactionReceipt` reply may be.
///
/// Sized from the biggest receipt this canister ever asks for, a CCTP v2 burn
/// (`depositForBurnWithHook`). That call emits three logs: the USDC `Transfer`,
/// `DepositForBurn` from the token messenger, and `MessageSent(bytes)` from the message
/// transmitter, whose payload is the whole 408-byte burn message (a 148-byte header, a
/// 228-byte burn body and the swap's 32-byte hook), which is 816 hex characters plus its
/// ABI offset and length words. With the receipt's own envelope, whose `logsBloom` alone
/// is 514 characters of JSON, and the two dozen scalar fields, that is about four
/// kilobytes. Eight is that with room for a fourth log.
///
/// An order that bridges on CCTP logs more than that: the pull, the fees, the burn's three
/// logs and the contracts' own. The ABI v2 router's measured above 8 KB on a fork and left
/// every read of it refused as too large until a replacement doubled the cap. So the cap
/// is 32 KB, four of those.
pub(crate) const MAX_RECEIPT_BYTES_PER_ITEM: u64 = 32_768;

/// How many receipts one outcall asks for. The cap an outcall reserves grows with this, and
/// a batch bigger than one outcall can hold is not an error the canister recovers from: the
/// system rejects the call, every later pass builds the same batch, and nothing on that
/// chain closes again. So the batch is split to fit rather than sized and hoped for.
const RECEIPTS_PER_CALL: usize = 32;

/// The most one `eth_sendRawTransaction` reply may be: a 32-byte hash as text, or a
/// JSON-RPC error object carrying a provider's message.
const MAX_SEND_BYTES_PER_ITEM: u64 = 1_024;

/// Why a decision on a chain cannot be made: the deploy listed no depth for it. A depth
/// nobody chose is not one to claim a deposit or close an attempt at, so the chain decides
/// nothing until the config names its depth.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum NoDepth {
    #[error("chain {chain_id} has no confirmation depth configured")]
    Unset { chain_id: ChainId },
    /// A chain listed in `finalized_chains`: a block count never decides it, so a site that
    /// asks for one stalls instead of deciding on a number nobody meant.
    #[error("chain {chain_id} waits for its finalized block: no block count decides it")]
    Finalized { chain_id: ChainId },
}

/// How a chain's transactions become final.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Finality {
    /// At block counts: the swap depth closes an attempt, the crediting depth lets an
    /// entry go.
    Blocks {
        swap: BlockDepth,
        credit: BlockDepth,
    },
    /// At the chain's own finalized block, for both.
    Finalized,
}

/// How `chain_id`'s transactions become final, or why the config says nothing of it.
pub(crate) fn finality(config: &types::Config, chain_id: ChainId) -> Result<Finality, NoDepth> {
    if config.finalized.contains(chain_id) {
        return Ok(Finality::Finalized);
    }
    let swap = confirmations(config, chain_id)?;
    // how long a closed entry keeps its bytes: never shallower than the swap depth, which
    // it falls back to
    let credit = credit_confirmations(config, chain_id).unwrap_or(swap);
    Ok(Finality::Blocks { swap, credit })
}

/// The highest block that is final on a chain, for closing an attempt (`swap`) and for
/// letting an entry go (`credit`), as one read showed it; None when the read could not say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Finals {
    pub swap: Option<BlockNumber>,
    pub credit: Option<BlockNumber>,
}

impl Finals {
    /// The finals under the head `latest` a receipt batch read, and the chain's finalized
    /// block read before it (`finalized`, a listed chain's only). A finalized block above
    /// that head is no reading at all.
    pub fn of(finality: Finality, latest: BlockNumber, finalized: Option<BlockNumber>) -> Self {
        match finality {
            Finality::Blocks { swap, credit } => Self {
                swap: Some(final_through(latest, swap)),
                credit: Some(final_through(latest, credit)),
            },
            Finality::Finalized => {
                let finalized = finalized.filter(|finalized| *finalized <= latest);
                Self {
                    swap: finalized,
                    credit: finalized,
                }
            }
        }
    }
}

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
    #[error(
        "a transaction on chain {chain_id} would spend {cost} on gas, above the chain's bound"
    )]
    GasCostTooHigh { chain_id: ChainId, cost: Wei },
    #[error("chain {chain_id} has no gas budget for a {action:?}")]
    NoGasBudget {
        chain_id: ChainId,
        action: GasAction,
    },
    #[error(
        "a {action:?} on chain {chain_id} could spend {cost} on gas, above its budget of {budget}"
    )]
    OverGasBudget {
        chain_id: ChainId,
        action: GasAction,
        cost: Wei,
        budget: Wei,
    },
    #[error("{0}: no signed transaction is recorded for it here")]
    NoRecord(&'static str),
    #[error(
        "a gas limit of {gas_limit} on chain {chain_id} is below the {least} its data needs to \
         be mined at all"
    )]
    GasBelowIntrinsic {
        chain_id: ChainId,
        gas_limit: GasAmount,
        least: u64,
    },
    #[error(
        "nonce {nonce} on chain {chain_id} was cancelled while swap {quote_hash}'s signature \
         was on its way, so the signed bytes carrying it are not recorded"
    )]
    NonceCancelled {
        quote_hash: QuoteHash,
        chain_id: ChainId,
        nonce: Nonce,
    },
    #[error(transparent)]
    Append(#[from] AppendError),
    #[error(transparent)]
    Ecdsa(#[from] EcdsaError),
}

/// Whether `quote_hash` still holds exactly `key` as its unsigned nonce: the signed bytes
/// carry that number, and a record that spent any other number the quote holds would put
/// two signed transactions on one nonce (the cancelled one) and strand the other. Pure, so
/// the race a signing await opens is proven without a signing subnet.
fn still_holds<S: Store>(
    state: &State<S>,
    quote_hash: &QuoteHash,
    key: NonceKey,
) -> Result<(), TxError> {
    if state.store().unsigned_nonce_of(quote_hash) == Some(key) {
        return Ok(());
    }
    Err(TxError::NonceCancelled {
        quote_hash: *quote_hash,
        chain_id: key.chain_id,
        nonce: key.nonce,
    })
}

/// The freshest reading of a chain, or the refusal that prices nothing: a transaction is
/// never sent on a reading older than `chain_data::READING_MAX_AGE`, because the fee it
/// would carry is the fee some earlier moment of the chain was asking. The cache only, with
/// no await: a call's reading or the canister's own (`gas::ensure_fresh`) is put there
/// before the caller's checks.
fn fresh_reading(chain_id: ChainId, now: Timestamp) -> Result<ChainData, TxError> {
    chain_data::fresh(chain_id, now).ok_or(TxError::StaleChainData { chain_id })
}

/// The fees to send at, from the freshest reading: the tip it suggests, and a
/// ceiling of twice the base fee on top of it, which carries a transaction through several
/// blocks of a rising base fee. A reading that prices a transaction above
/// [`MAX_FEE_PER_GAS`] prices nothing: the bound is what a fee this canister did not compute
/// is held to.
fn fees(chain_id: ChainId, now: Timestamp) -> Result<Fees, TxError> {
    let ceiling = max_fee_per_gas(chain_id);
    let out_of_range = TxError::FeeOutOfRange { chain_id, ceiling };
    let fees = fresh_reading(chain_id, now)?
        .fees()
        .ok_or(out_of_range.clone())?;
    if fees.max_fee() > ceiling || fees.max_priority_fee() > ceiling {
        return Err(out_of_range);
    }
    Ok(fees)
}

/// Refuses a transaction whose gas could cost more than this canister will ever spend on
/// one. The per-gas ceiling bounds the price and this bounds the price times the limit, so
/// a gas limit nobody checked cannot turn a sane price into an insane bill.
fn affordable(chain_id: ChainId, fees: Fees, gas_limit: GasAmount) -> Result<(), TxError> {
    let cost = fees.worst_cost(gas_limit).ok_or(TxError::GasCostTooHigh {
        chain_id,
        cost: Wei::MAX,
    })?;
    if cost > max_transaction_cost(chain_id) {
        return Err(TxError::GasCostTooHigh { chain_id, cost });
    }
    Ok(())
}

/// The gas budget of `action` on `chain_id`, or the refusal when the chain is not one the
/// canister signs on. A cancel has no action and no budget: it spends a nonce that cannot
/// be left unspent, and [`max_transaction_cost`] bounds it.
pub(crate) fn budget_for(
    config: &types::Config,
    chain_id: ChainId,
    action: GasAction,
) -> Result<Wei, TxError> {
    config
        .evm_chains
        .get(chain_id)
        .and_then(|chain| chain.gas_budget.get(action))
        .ok_or(TxError::NoGasBudget { chain_id, action })
}

/// The gas rule as `create_and_send` holds a transaction to it, read from `config`: the
/// chain signs this action, and the limit times the fee cap is within its budget.
pub fn gas_rule(
    config: &types::Config,
    chain_id: ChainId,
    action: GasAction,
    fees: Fees,
    gas_limit: GasAmount,
) -> Result<(), TxError> {
    let budget = budget_for(config, chain_id, action)?;
    within_budget(chain_id, action, budget, fees, gas_limit)
}

/// The gas rule (spec 3.3): the most a transaction can spend on gas, its limit times its
/// fee cap, is within the budget of its action on its chain.
pub fn within_budget(
    chain_id: ChainId,
    action: GasAction,
    budget: Wei,
    fees: Fees,
    gas_limit: GasAmount,
) -> Result<(), TxError> {
    let cost = fees.worst_cost(gas_limit).unwrap_or(Wei::MAX);
    if cost > budget {
        return Err(TxError::OverGasBudget {
            chain_id,
            action,
            cost,
            budget,
        });
    }
    Ok(())
}

/// Where the one transaction a door signs under `key` stands: signed already (its id in
/// `signed_txs`), being signed, or not yet. What makes a door that signs once per key
/// answer the same id to the same call.
pub fn signed_once(key: QuoteHash) -> Result<Option<u64>, String> {
    read_state(|state| {
        if let Some(flow) = state.flow(&key) {
            return flow
                .signed_id
                .map(Some)
                .ok_or_else(|| format!("{key} has a flow but no signed transaction"));
        }
        if state.store().has_unsigned_nonce_for(&key) {
            return Err(format!("{key} is being signed"));
        }
        Ok(None)
    })
}

/// Whether a transaction of `purpose` is being signed: a nonce held under its key. Never for
/// a purpose with no key (a cancel).
pub fn being_signed(purpose: TxPurpose) -> bool {
    purpose
        .quote_hash()
        .is_some_and(|key| read_state(|state| state.store().has_unsigned_nonce_for(&key)))
}

/// Whether a transaction of `purpose`, one of the pool's own, is being signed (its nonce
/// held) or is in the outbox. A purpose with no key (a cancel) is none of the pool's: false.
pub fn in_flight(purpose: TxPurpose) -> bool {
    purpose.quote_hash().is_some()
        && (being_signed(purpose) || crate::storage::outbox::find(purpose).is_some())
}

/// What a flow's or a pool payout's signed record says beyond the transaction itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Detail {
    /// The order a pool payout is signed with, pinned for a retry after a revert.
    PoolPayout { order: types::pool::PinnedOrder },
    /// The orphan a pool transfer refunds, to whom, and what it sends.
    PoolTransfer {
        key: types::pool::CreditKey,
        to: EvmAddress,
        sent: types::TokenAmount,
    },
    /// What a CCTP finish does, the keccak of its message, and the source transaction
    /// that burned it.
    Finish {
        action: types::flow::FinishAction,
        arrival: [u8; 32],
        source_tx: TxHash,
        /// How many arrivals the source transaction paid for.
        paid: u32,
    },
}

/// A transaction signed and queued: its hash, and the log index of the line that recorded
/// its bytes, which is its id in `signed_txs`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Signed {
    pub tx_hash: TxHash,
    pub id: EventIndex,
}

/// The line a signed transaction is recorded by: a flow's step or the pool's own, each
/// holding the nonce its `TxCreated` handed out.
enum Record {
    /// A gasless relay's, which creates its flow.
    Relay,
    /// A CCTP finish's, which creates its flow.
    Finish {
        action: types::flow::FinishAction,
        arrival: [u8; 32],
        source_tx: TxHash,
        paid: u32,
    },
    /// A pool payout's, which moves it to signed.
    PoolPayout {
        id: u64,
        order: types::pool::PinnedOrder,
    },
    /// An orphan's refund.
    PoolTransfer {
        key: types::pool::CreditKey,
        to: EvmAddress,
        sent: types::TokenAmount,
    },
    /// The canister's allowance to Swapic.
    PoolApprove,
    /// A rescue's, naming the order whose intent it cancels.
    EcoRescue([u8; 32]),
    /// A move's, which moves it to signed.
    PoolMove(u64),
    /// A controller's EVM sweep's.
    Sweep(u64),
    /// A controller's claim of the canister's own deferred Across refunds.
    AcrossClaim,
}

/// The record a signed transaction of `purpose` is written in, decided before its nonce is
/// handed out, pure. Refused for a purpose this path records nothing for: a cancel (the pass
/// that ends a stranded allocation signs and records it), the retired v1 swap legs, pulls
/// and continuations, and a finish, a pool payout or a pool transfer handed no detail.
fn record_of(purpose: TxPurpose, detail: Option<Detail>) -> Result<Record, TxError> {
    match (purpose, detail) {
        (TxPurpose::Relay(_), _) => Ok(Record::Relay),
        (
            TxPurpose::Finish(_),
            Some(Detail::Finish {
                action,
                arrival,
                source_tx,
                paid,
            }),
        ) => Ok(Record::Finish {
            action,
            arrival,
            source_tx,
            paid,
        }),
        (TxPurpose::Finish(_), _) => Err(TxError::NoRecord("a finish with no detail")),
        (TxPurpose::PoolPayout(id), Some(Detail::PoolPayout { order })) => {
            Ok(Record::PoolPayout { id, order })
        }
        (TxPurpose::PoolPayout(_), _) => Err(TxError::NoRecord("a pool payout with no order")),
        (TxPurpose::PoolTransfer(_), Some(Detail::PoolTransfer { key, to, sent })) => {
            Ok(Record::PoolTransfer { key, to, sent })
        }
        (TxPurpose::PoolTransfer(_), _) => Err(TxError::NoRecord("a pool transfer with no orphan")),
        (TxPurpose::PoolApprove, _) => Ok(Record::PoolApprove),
        (TxPurpose::EcoRescue(order_id), _) => Ok(Record::EcoRescue(order_id)),
        (TxPurpose::PoolMove(id), _) => Ok(Record::PoolMove(id)),
        (TxPurpose::Cancel(_), _) => Err(TxError::NoRecord("a cancel")),
        (TxPurpose::Sweep(id), _) => Ok(Record::Sweep(id)),
        (TxPurpose::AcrossClaim(_), _) => Ok(Record::AcrossClaim),
    }
}

impl Record {
    /// The line itself, once the bytes are signed: they carry `nonce` on `chain_id`.
    fn line(
        self,
        quote_hash: QuoteHash,
        chain_id: ChainId,
        nonce: Nonce,
        tx_hash: TxHash,
        raw_tx: Vec<u8>,
    ) -> EventType {
        use types::pool::PoolStep;
        let flow = |step| EventType::Flow { quote_hash, step };
        let pool = |step| EventType::Pool { step };
        match self {
            Record::Relay => flow(types::FlowStep::GaslessRelayed {
                chain_id,
                nonce,
                tx_hash,
                raw_tx,
            }),
            Record::Finish {
                action,
                arrival,
                source_tx,
                paid,
            } => flow(types::FlowStep::CctpFinishSigned {
                chain_id,
                nonce,
                tx_hash,
                raw_tx,
                action,
                arrival,
                source_tx: Some(source_tx),
                paid: Some(paid),
            }),
            Record::PoolPayout { id, order } => pool(PoolStep::PayoutSigned {
                id,
                order,
                chain_id,
                nonce,
                tx_hash,
                raw_tx,
            }),
            Record::PoolTransfer { key, to, sent } => pool(PoolStep::OrphanRefundSigned {
                key,
                to,
                sent,
                chain_id,
                nonce,
                tx_hash,
                raw_tx,
            }),
            Record::PoolApprove => pool(PoolStep::ApproveSigned {
                chain_id,
                nonce,
                tx_hash,
                raw_tx,
            }),
            Record::EcoRescue(order_id) => pool(PoolStep::EcoRescueSigned {
                order_id,
                chain_id,
                nonce,
                tx_hash,
                raw_tx,
            }),
            Record::PoolMove(id) => pool(PoolStep::MoveSigned {
                id,
                chain_id,
                nonce,
                tx_hash,
                raw_tx,
            }),
            Record::Sweep(id) => pool(PoolStep::SweepSigned {
                id,
                chain_id,
                nonce,
                tx_hash,
                raw_tx,
            }),
            Record::AcrossClaim => pool(PoolStep::AcrossClaimSigned {
                chain_id,
                nonce,
                tx_hash,
                raw_tx,
            }),
        }
    }
}

/// Creates, signs and queues one transaction, and answers the hash it will be looked up
/// by.
///
/// The order is the law of this module: halt check, fees, `TxCreated` with no await before
/// it, signature, the signed record, queue. The broadcast itself is the outbox pass, so the caller
/// does not wait on a provider to know its nonce is safely allocated.
///
/// `action` names the gas budget the transaction is held to (the gas rule), here and in
/// every replacement of it.
pub async fn create_and_send(
    purpose: TxPurpose,
    chain_id: ChainId,
    to: EvmAddress,
    value: Wei,
    data: Vec<u8>,
    gas_limit: GasAmount,
    action: GasAction,
) -> Result<Signed, TxError> {
    create_and_send_with(purpose, chain_id, to, value, data, gas_limit, action, None).await
}

/// The least gas a transaction carrying `data` needs before a node takes it (no access list):
/// 21,000 plus the larger of EIP-2028's calldata cost and EIP-7623's floor, with tokens
/// counted as zero bytes plus four per nonzero byte.
pub fn least_gas(data: &[u8]) -> u64 {
    let zero = data.iter().filter(|byte| **byte == 0).count() as u64;
    let tokens = zero + 4 * (data.len() as u64 - zero);
    21_000 + (4 * tokens).max(10 * tokens)
}

/// [`create_and_send`] for a flow whose signed record carries a [`Detail`].
#[allow(clippy::too_many_arguments)]
pub async fn create_and_send_with(
    purpose: TxPurpose,
    chain_id: ChainId,
    to: EvmAddress,
    value: Wei,
    data: Vec<u8>,
    gas_limit: GasAmount,
    action: GasAction,
    detail: Option<Detail>,
) -> Result<Signed, TxError> {
    require_not_halted().map_err(TxError::Guard)?;
    // the record its signed bytes will be written in, decided before any number is handed
    // out: a purpose with none here is refused with nothing allocated
    let record = record_of(purpose, detail)?;
    let quote_hash = purpose.quote_hash().ok_or(TxError::NoRecord("a cancel"))?;
    // a limit no node takes would hold its nonce, and every later transaction on the chain,
    // for good: refused before any number is handed out
    let least = least_gas(&data);
    if gas_limit < GasAmount::from(least) {
        return Err(TxError::GasBelowIntrinsic {
            chain_id,
            gas_limit,
            least,
        });
    }
    let now = Timestamp::from_nanos(ic_cdk::api::time());
    let fees = fees(chain_id, now)?;
    affordable(chain_id, fees, gas_limit)?;
    gas_rule(&config::get(), chain_id, action, fees, gas_limit).inspect_err(|error| {
        ic_cdk::println!("not signed for {quote_hash}: {error}");
    })?;

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
        action: Some(action),
    })?;

    // the one place this call arms the pass: here, after the allocation and before the
    // first await, so every way it can fail from now on still leaves a pass scheduled to
    // end the allocation, and a trap before the await rolls the append back with it, so
    // an unsigned nonce and an unarmed pass cannot both happen. Nothing arms again after
    // the queue below, because the pass re-arms itself for as long as this nonce is
    // unsigned or the outbox holds anything, and the entry is queued before either of
    // those stops being true.
    task_manager::outbox::arm();

    // the signature is the await this whole order exists for. From here on a failure
    // leaves the nonce in the fold as created-but-unsigned, and the outbox pass spends it
    // with a cancel rather than leaving the account with a gap (A5).
    let signature = ecdsa::sign(tx.signing_hash()).await?;
    let signed = tx.signed(signature);
    let tx_hash = signed.hash();
    let raw_tx = signed.into_raw();
    // the bytes carry the number that was allocated before the await, so the key must still
    // hold exactly that number, checked here with no await between the check and the
    // append. A cancel that spent the number meanwhile has sealed it, and a retry may have
    // handed the key a fresh one; either way the late signature is refused, the outbox is
    // not touched, and the cancel's entry at this nonce stays exactly as the pass queued it
    read_state(|state| still_holds(state, &quote_hash, NonceKey { chain_id, nonce }))?;
    let record = record.line(quote_hash, chain_id, nonce, tx_hash, raw_tx.clone());
    let id = append_event(record)?;
    outbox::put(OutboxEntry {
        purpose,
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
        to,
        value,
        // the calldata was copied once, into the log; the transaction's own moves on here
        data: tx.data,
        gas_limit,
        refusal: None,
        closed_at: None,
        closed_success: None,
        action: Some(action),
    });
    Ok(Signed { tx_hash, id })
}

/// The gas a bare transfer of the chain's own currency costs, which is what a cancel is.
/// Fixed by the EVM itself, so no config knob can move it.
const CANCEL_GAS_LIMIT: u64 = 21_000;

/// Spends every nonce that was handed out and never signed for, with a zero-value transfer
/// from this canister's address to itself at that exact number (rule A5).
///
/// A nonce leaves the allocator with `TxCreated` and is spent by its signed record. Anything
/// that refuses in between, a rejected signing call, a signed record the fold no longer admits,
/// a trap or an upgrade mid-await, leaves the number handed out with nothing carrying it,
/// and an EVM account with a gap at N can never mine N+1: every payout, refund and burn on
/// that chain stops. So the allocation is in the fold, and this pass ends it.
///
/// An allocation is stranded once it is older than [`STRANDED_AFTER`], which is a signing
/// round trip and then some, never the batch window: the path from the append to the
/// signed record is not one message chain, its signing await spans consensus rounds on
/// the signing subnet, and a cancel fired inside that wait races the very signature it
/// stands in for. Which allocations are stranded is judged at one instant, the pass's
/// start; each cancel then reads the clock for itself, because the awaits between two
/// cancels are signing round trips of their own. Signing and broadcasting a cancel is
/// creating a transaction, so the halt switch gates it like every other such path.
pub async fn cancel_stranded() {
    if require_not_halted().is_err() {
        return;
    }
    let now = Timestamp::from_nanos(ic_cdk::api::time());
    let stranded: Vec<NonceKey> = read_state(|state| {
        state
            .unsigned_nonces()
            .into_iter()
            .filter(|(_, unsigned)| unsigned.is_stranded(now, STRANDED_AFTER))
            .map(|(key, _)| key)
            .collect()
    });
    for key in stranded {
        cancel(key).await;
    }
}

/// What a cancel on `chain_id` pays at `now`: the same door every other send is priced
/// through, so it carries the same ceilings, and a bad or aged reading refuses it rather
/// than putting a stale or a huge fee on the chain.
fn cancel_fees(chain_id: ChainId, now: Timestamp) -> Result<Fees, TxError> {
    let fees = fees(chain_id, now)?;
    affordable(chain_id, fees, GasAmount::from(CANCEL_GAS_LIMIT))?;
    Ok(fees)
}

/// Signs, records and queues the cancel of one allocated nonce. Recorded before it is
/// broadcast like every other transaction (A6), and the record is what seals the nonce's
/// fate: the entry then rides the outbox as any transaction does, and its receipt needs no
/// further line in the log.
///
/// The clock is read here and not once for the whole pass: the cancels before this one
/// each awaited a signature, so the pass's instant is stale by a round trip per cancel,
/// which would price this one on a reading that has aged out and stamp its entry with an
/// instant before it existed.
async fn cancel(key: NonceKey) {
    let NonceKey { chain_id, nonce } = key;
    // no call brings a cancel a reading: the canister reads the chain's gas itself
    if crate::gas::ensure_fresh(chain_id).await.is_err() {
        return;
    }
    let now = Timestamp::from_nanos(ic_cdk::api::time());
    let gas_limit = GasAmount::from(CANCEL_GAS_LIMIT);
    let Ok(fees) = cancel_fees(chain_id, now) else {
        return;
    };
    let Ok(mine) = ecdsa::canister_address().await else {
        return;
    };
    // the fold may have moved while the address was being read: a signed record that arrived
    // late spends this number, and cancelling it would put a second transaction on it
    let Some(purpose) =
        read_state(|state| state.store().unsigned_nonce(&key)).map(|unsigned| unsigned.purpose)
    else {
        return;
    };
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
    let signed = tx.signed(signature);
    let tx_hash = signed.hash();
    let raw_tx = signed.into_raw();
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
    // a sweep whose bytes were never sent moved nothing: its amount goes back on its books
    if let TxPurpose::Sweep(id) = purpose {
        crate::sweep::evm::taken_back(id, "its nonce was cancelled", now);
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
        refusal: None,
        closed_at: None,
        closed_success: None,
        action: None,
    });
    task_manager::outbox::arm();
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

/// Hands every queued transaction to its chain's provider, one batch per chain. The nonce
/// is allocated either way, so the only way out is forward, and every answer a provider
/// gives moves the entry on: it stays queued only when no answer came back at all.
///
/// A result, a provider that already holds the transaction, and a nonce the chain calls
/// too low (one of this entry's EARLIER transactions is already mined) all say there is
/// nothing left to broadcast. Any other refusal is a hint like any other (rule A10): the
/// bytes were offered to the chain and may well be in a mempool, so the entry is watched
/// all the same, with the refusal kept on it, and the rebroadcast and replacement rules
/// take it from there. A refusal for the price ends in a bump; one the chain will never
/// accept ends at the fee ceiling with the bytes still going out, which is the rule the
/// outbox has for a transaction the chain will not mine. Left queued instead, the same
/// bytes would be refused every window forever and every later nonce on the chain would
/// wait behind them.
pub async fn flush() {
    // the emergency stop: a halted canister hands nothing new to a provider, whether it was
    // queued before the halt or re-queued during it. Nothing is abandoned, because the
    // entry keeps its nonce and its bytes and goes out when the halt lifts.
    if require_not_halted().is_err() {
        return;
    }
    let now = Timestamp::from_nanos(ic_cdk::api::time());
    let config = config::get();
    let limit = config.max_batch_items as usize;
    for chain_id in outbox::chains() {
        let batch = outbox::by_status(chain_id, OutboxStatus::Queued, limit);
        if batch.is_empty() {
            continue;
        }
        let mut went_out = false;
        if config.private_send.contains(chain_id) {
            // BSC design 6.2.13: the watcher sends this chain's bytes through its private node,
            // so no searcher sees them before they land; the canister makes no outcall, marks
            // the batch sent, and reads its receipts, bumps and cancels as on any chain
            for entry in batch {
                if let Some(mut current) = outbox::get(entry.key()) {
                    if current.tx_hash() == entry.tx_hash() {
                        current.sent(now);
                        went_out = true;
                        outbox::put(current);
                    }
                }
            }
        } else {
            let calls: Vec<(&str, Value)> = batch
                .iter()
                .map(|entry| ("eth_sendRawTransaction", json!([hex0x(&entry.raw_tx)])))
                .collect();
            // the cap is the batch's own, so a `max_batch_items` a controller raised cannot
            // put a batch on the wire that the answer will not fit inside
            let cap = send_cap(calls.len());
            let Ok(answers) = rpc::rpc_batch_each(chain_id, &calls, cap).await else {
                // the provider is unreachable: every entry stays queued
                continue;
            };
            for (entry, answer) in batch.into_iter().zip(answers) {
                let refusal = match answer {
                    Ok(_) => None,
                    Err(RpcError::Rpc { message, .. })
                        if already_on_the_network(&message) || nonce_already_spent(&message) =>
                    {
                        None
                    }
                    Err(RpcError::Rpc { message, .. }) => Some(message),
                    // no answer to this call: the bytes are offered again next window
                    Err(_) => continue,
                };
                // A3: the entry is the one this pass read, and the only fields it moves are
                // how far the broadcast got and what the provider said
                if let Some(mut current) = outbox::get(entry.key()) {
                    if current.tx_hash() == entry.tx_hash() {
                        match refusal {
                            Some(message) => current.refused(now, &message),
                            None => {
                                current.sent(now);
                                went_out = true;
                            }
                        }
                        outbox::put(current);
                    }
                }
            }
        }
        // bytes that just went out are worth a read a block from now, whatever the chain's
        // other entries wait for
        if went_out {
            let read = next_receipt_read(now, block_time(&config, chain_id), 1);
            timer_state::update(|state| {
                let due = state.receipts_due.entry(chain_id).or_insert(read);
                let sooner = *due > read;
                if sooner {
                    *due = read;
                }
                ((), sooner)
            });
        }
    }
}

/// Keeps a finalized-tag chain's last finalized block and when it last moved
/// (`TimerState.finalized_seen`, read by `finalized_seen`), from a read that gave `read` or
/// failed; a stall raises one ALERT line. Nothing moves while it stands: no entry closes.
/// `resumed`: the chain waits again after the last pass found nothing open on it, and its
/// clock starts at this read.
fn note_finalized(chain_id: ChainId, read: Option<BlockNumber>, now: Timestamp, resumed: bool) {
    let alert = timer_state::update(|state| {
        let seen = state
            .finalized_seen
            .entry(chain_id)
            .or_insert_with(|| types::timer_state::FinalizedSeen::first(now));
        if resumed {
            seen.restart(now);
        }
        let alert = seen.observe(read, now);
        (alert.then_some(*seen), true)
    });
    if let Some(seen) = alert {
        log(&format!(
            "ALERT: chain {chain_id}'s finalized block {} has not moved for {} minutes: nothing on it closes until it does",
            seen.number.map_or_else(|| "(never read)".to_string(), |number| number.to_string()),
            now.saturating_duration_since(seen.moved_at).as_secs() / 60
        ));
    }
}

/// A chain's block time as the config names it, and the outbox's own floor where it names
/// none.
fn block_time(config: &types::Config, chain_id: ChainId) -> Duration {
    config
        .evm_chains
        .0
        .get(&chain_id)
        .map_or(MIN_RECEIPT_PACE, |chain| chain.block_time)
}

/// What one receipt pass did. Returned rather than logged, so a pass can be read without the
/// event log.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReceiptPass {
    /// attempts closed on a receipt deep enough, confirmed or reverted
    pub closed: usize,
    /// chunks whose answer did not come back whole and were read again one entry at a time
    pub reread: usize,
    /// entries whose own answer did not come back either, left to the rebroadcast rule
    pub unread: usize,
    /// entries on a chain the config lists no depth for: nothing about them is decided
    /// until a deploy names that chain's depth, and they stay in the outbox meanwhile
    pub no_depth: usize,
    /// entries on a chain whose next read is not due yet
    pub waiting: usize,
}

/// Reads what happened to every transaction still out, one batch per chain: the head
/// block, then a receipt for every hash ever broadcast at each open nonce. A receipt deep
/// enough closes the attempt, a reverted one at the same depth fails it, and a transaction
/// that is not landing is re-sent and then replaced.
///
/// A chunk whose answer does not come back whole, one reply too big for the cap, a
/// provider error, a head that does not parse, is read again one entry at a time, so one
/// bad answer stalls nothing but its own entry. An entry whose own answer does not come
/// back either is handed to the rebroadcast rule, the way an entry no receipt came back
/// for is, and counted: left where it is, every pass would build the same chunk, be
/// refused the same answer, and nothing on that chain would close again.
///
/// This is the read that decides money: the lines that close an attempt are appended from
/// what it answers.
// todo_harden_reads: single unreplicated read; upgrade to k-of-n later. Until then the
// answers are bound to what this canister signed (`parse_receipt` refuses a receipt for a
// hash this entry never broadcast) and held to the configured depth, so one provider can
// delay a decision but cannot invent one.
pub async fn check_open() -> ReceiptPass {
    let now = Timestamp::from_nanos(ic_cdk::api::time());
    let config = config::get();
    let limit = config.max_batch_items as usize;
    let mut pass = ReceiptPass::default();
    let due = timer_state::get().receipts_due;
    // when each chain with entries out is read next; a chain with none drops out
    let mut next: BTreeMap<ChainId, Timestamp> = BTreeMap::new();
    for chain_id in outbox::chains() {
        let open = outbox::by_status(chain_id, OutboxStatus::Sent, limit);
        if open.is_empty() {
            continue;
        }
        // a chain read at its pace: nothing about its entries can have changed yet
        if let Some(at) = due.get(&chain_id).filter(|at| **at > now) {
            next.insert(chain_id, *at);
            pass.waiting += open.len();
            continue;
        }
        let pace = block_time(&config, chain_id);
        // a chain with no depth configured closes nothing: the entries wait in the outbox,
        // rebroadcast by their own clock, until a deploy names the depth to judge them at
        let Ok(finality) = finality(&config, chain_id) else {
            pass.no_depth += open.len();
            next.insert(chain_id, next_receipt_read(now, pace, 1));
            continue;
        };
        // a listed chain's finalized block, read in a call of its own before the receipts
        // (so it is never above their head): a failed read leaves every landed entry waiting
        // and sends again only the ones with no receipt, exactly as the receipts say. It is
        // read at its own pace; between reads the last number read stands, which only
        // closes later, never wrongly
        let finalized = match finality {
            Finality::Finalized => {
                // a chain the last pass found nothing open on (out of its schedule) waits
                // again from now
                let resumed = !due.contains_key(&chain_id);
                let seen = timer_state::get().finalized_seen.get(&chain_id).copied();
                match seen.filter(|seen| !resumed && !seen.read_due(now)) {
                    Some(seen) => seen.number,
                    None => {
                        let read = rpc::finalized(chain_id).await.ok();
                        note_finalized(chain_id, read, now, resumed);
                        read
                    }
                }
            }
            Finality::Blocks { .. } => None,
        };
        // the fewest blocks any entry waits before a read can decide something about it
        let mut wait = u64::MAX;
        let mut waits = |entry: &OutboxEntry, landed: Option<&Receipt>, latest: BlockNumber| {
            let blocks = match finality {
                Finality::Blocks { swap, credit } => {
                    let target = if entry.closed_at.is_some() {
                        credit
                    } else {
                        swap
                    };
                    blocks_to_wait(landed.map(|receipt| receipt.block), latest, target)
                }
                // the finalized block moves every few blocks: one block on, a read may
                // decide something
                Finality::Finalized => 1,
            };
            wait = wait.min(blocks);
        };
        // split so the answer fits one outcall: the head comes with every chunk, so each
        // chunk's receipts are read against a height from the same answer
        for chunk in receipt_chunks(open) {
            match read_receipts(chain_id, &chunk).await {
                Ok((latest, landed)) => {
                    for (entry, landed) in chunk.iter().zip(landed) {
                        waits(entry, landed.as_ref(), latest);
                        pass.closed += decide(
                            entry,
                            chain_id,
                            landed,
                            Finals::of(finality, latest, finalized),
                            now,
                        )
                        .await;
                    }
                }
                // only a failed call is worth splitting: a head that did not come back as
                // a number fails every chunk of any size the same way
                Err(Unread::Rpc) if chunk.len() > 1 => {
                    pass.reread += 1;
                    for entry in &chunk {
                        match read_receipts(chain_id, std::slice::from_ref(entry)).await {
                            Ok((latest, landed)) => {
                                let landed = landed.into_iter().next().flatten();
                                waits(entry, landed.as_ref(), latest);
                                pass.closed += decide(
                                    entry,
                                    chain_id,
                                    landed,
                                    Finals::of(finality, latest, finalized),
                                    now,
                                )
                                .await;
                            }
                            Err(_) => {
                                pass.unread += 1;
                                waits(entry, None, BlockNumber::new(0));
                                push_again(entry, chain_id, now).await;
                            }
                        }
                    }
                }
                // a chunk of one: its answer is the entry's own
                Err(_) => {
                    pass.unread += chunk.len();
                    for entry in &chunk {
                        waits(entry, None, BlockNumber::new(0));
                        push_again(entry, chain_id, now).await;
                    }
                }
            }
        }
        next.insert(chain_id, next_receipt_read(now, pace, wait));
    }
    // a chain whose last entry this pass closed has nothing to read: no pass comes back for it,
    // so it must not stay in the schedule, or an idle chain reads as one that waits
    next.retain(|chain_id, _| !outbox::by_status(*chain_id, OutboxStatus::Sent, 1).is_empty());
    timer_state::update(|state| {
        let changed = state.receipts_due != next;
        state.receipts_due = next;
        ((), changed)
    });
    pass
}

/// Why a chunk's answer decided nothing about any entry in it. A failed call is re-read one
/// entry at a time, because the chunk's size may be what failed it; a head that is not a
/// number leaves every entry to the rebroadcast rule, because no smaller chunk would read
/// it better.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Unread {
    /// the call itself failed: unreachable, refused by the system, a provider error, or an
    /// answer that is not the batch asked for
    Rpc,
    /// the head block did not come back as a number, so no receipt has a depth
    NoHead,
}

/// The head and every receipt of `entries` in one outcall, answered as the head and the
/// receipt that landed for each entry, in order.
async fn read_receipts(
    chain_id: ChainId,
    entries: &[OutboxEntry],
) -> Result<(BlockNumber, Vec<Option<Receipt>>), Unread> {
    let mut calls: Vec<(&str, Value)> = vec![("eth_blockNumber", json!([]))];
    for entry in entries {
        for hash in &entry.hashes {
            calls.push(("eth_getTransactionReceipt", json!([hex0x(hash.as_ref())])));
        }
    }
    let cap = receipt_cap(calls.len() - 1);
    let answers = rpc::rpc_batch_each(chain_id, &calls, cap)
        .await
        .map_err(|_| Unread::Rpc)?;
    let fee_path = config::with(|config| {
        config
            .evm_chains
            .get(chain_id)
            .zip(crate::storage::ecdsa_address::get())
            .map(|(chain, canister)| crate::books::FeePath {
                usdc: chain.usdc,
                runner: chain.runner,
                canister,
                eco_portal: config.eco_portal,
            })
    });
    // the chain's SpokePool pin, where a payout's own Across deposit is read
    let across_pin = config::with(|config| {
        config
            .evm_chains
            .get(chain_id)
            .and_then(|chain| chain.across)
            .zip(crate::storage::ecdsa_address::get())
            .map(|(pins, canister)| AcrossPin {
                chain: chain_id,
                spoke_pool: pins.spoke_pool,
                canister,
            })
    });
    landed(entries, &answers, fee_path, across_pin).ok_or(Unread::NoHead)
}

/// The head and the receipt that landed for each entry, sliced out of one chunk's answers
/// in call order: `rpc_batch_each` answers exactly one result per call, so the head is at
/// 0 and each entry's receipts follow in the order the calls were built. Nothing at all
/// when the head is not a number, or the answers are fewer than the calls.
fn landed(
    entries: &[OutboxEntry],
    answers: &[Result<Value, RpcError>],
    fee_path: Option<crate::books::FeePath>,
    across_pin: Option<AcrossPin>,
) -> Option<(BlockNumber, Vec<Option<Receipt>>)> {
    let latest = answers
        .first()?
        .as_ref()
        .ok()?
        .as_str()
        .and_then(parse_block_number)?;
    let mut receipts_from = 1;
    let mut landed = Vec::with_capacity(entries.len());
    for entry in entries {
        let receipts = answers.get(receipts_from..receipts_from + entry.hashes.len())?;
        receipts_from += entry.hashes.len();
        landed.push(
            receipts
                .iter()
                .filter_map(|answer| answer.as_ref().ok())
                .find_map(|value| {
                    let receipt = parse_receipt(value, &entry.hashes).ok()?;
                    Some(Receipt {
                        network_fee: fee_path.map_or(types::TokenAmount::ZERO, |path| {
                            crate::books::network_fee_in(value, path)
                        }),
                        eco_intent: fee_path.and_then(|path| eco_intent_in(value, &path)),
                        across: across_pin.map(|pin| {
                            crate::pool::across::deposit_in(
                                &crate::pool::arrival::logs_of(value),
                                pin.spoke_pool,
                                pin.canister,
                                pin.chain,
                            )
                        }),
                        ..receipt
                    })
                }),
        );
    }
    Some((latest, landed))
}

/// Acts on what one entry's answer said, and answers how many attempts it closed: one when
/// a receipt at the swap depth `depth` closed the attempt, none while a receipt waits for
/// depth, and none when no receipt came back, in which case the transaction is not landing
/// and is re-sent and then replaced.
///
/// An entry whose attempt already closed is kept only for its bytes, until its receipt is
/// at the crediting depth `credit`, and is never decided again: it is dropped at that
/// depth, and while kept nothing is appended for it. A receipt that is gone (a reorg
/// dropped the transaction, or a lagging node has not seen it) sends the SAME bytes again
/// on the rebroadcast cadence, and never a replacement: new bytes would be a new signature
/// and a `TxReplaced` line for an attempt the log already closed. Nor is it ever cancelled:
/// a cancel spends only a nonce nothing was signed for. A receipt that comes back with the
/// other outcome than the one the attempt closed on (a reorg turned it round) is reported
/// in the canister's log, one line per pass while kept, and changes nothing.
///
/// Why a retry signed after a failed close cannot pay twice when a reorg then turns the
/// failure into a success, today: every retry carries the same order id or the same CCTP
/// message as the transaction it follows. Swapic refuses an order id it has run
/// (`used[id]`), and Circle's transmitter refuses a message whose nonce it has received,
/// so of the two at most one does anything. Any new purpose the canister signs and retries
/// after a failed close must carry such an id as well.
async fn decide(
    entry: &OutboxEntry,
    chain_id: ChainId,
    landed: Option<Receipt>,
    finals: Finals,
    now: Timestamp,
) -> usize {
    if entry.closed_at.is_some() {
        match landed {
            Some(receipt) => {
                if let Some(line) = divergence(entry, &receipt) {
                    log(&line);
                }
                if is_final(receipt.block, finals.credit) {
                    sweep_settled(entry, receipt.success, now);
                    outbox::remove(entry.key());
                }
            }
            None => rebroadcast(entry, now),
        }
        return 0;
    }
    match landed {
        // rule A10: only a receipt at the configured depth closes an attempt, and that
        // holds however the transaction ended. A reverted receipt closed at once would
        // free the swap to be re-signed at a fresh nonce, and a one-block reorg that drops
        // the revert and includes the replacement then pays twice.
        Some(receipt) if is_final(receipt.block, finals.swap) => {
            let Receipt {
                tx_hash,
                block,
                success,
                network_fee,
                eco_intent,
                across,
            } = receipt;
            // an orphan's refund closes with its own line: refunded, or held again
            if let TxPurpose::PoolTransfer(hash) = entry.purpose {
                let refunding = read_state(|state| {
                    state
                        .store()
                        .pool_orphans()
                        .into_iter()
                        .find(|(key, orphan)| {
                            types::pool::orphan_key(key) == hash
                                && matches!(
                                    orphan.status,
                                    types::pool::OrphanStatus::Refunding { .. }
                                )
                        })
                });
                let appended = match refunding {
                    Some((key, _)) => crate::storage::pool::record_at(
                        types::pool::PoolStep::OrphanRefundClosed { key, success },
                        now,
                    )
                    .is_ok(),
                    // closed already: nothing left to record
                    None => true,
                };
                if appended {
                    settle(entry, tx_hash, block, success, finals.credit, now);
                }
                return usize::from(appended);
            }
            // a rescue is logged as it closes: Eco refunds the creator on the source by itself
            if let TxPurpose::EcoRescue(order_id) = entry.purpose {
                log(&format!(
                    "rescue: the cancelAndProve of order 0x{}'s intent {} at block {block} on chain {chain_id}",
                    hex::encode(order_id),
                    if success { "succeeded" } else { "reverted" },
                ));
            }
            // a move's or a pool payout's transaction closes with its own line: burned or
            // paid (with the Eco intent its order published, whose refund routes the move
            // again or credits the pool), or reverted (a move signs the same order again, a
            // payout is undone once its order is known unused). It paid the canister no fee,
            // so none is booked
            let pool_close = match entry.purpose {
                TxPurpose::PoolMove(id) if success => Some((
                    format!("the move {id}"),
                    types::pool::PoolStep::MoveBurned {
                        id,
                        tx_hash,
                        block,
                        intent: eco_intent,
                    },
                )),
                TxPurpose::PoolMove(id) => Some((
                    format!("the move {id}"),
                    types::pool::PoolStep::MoveReverted {
                        id,
                        block: Some(block),
                    },
                )),
                TxPurpose::PoolPayout(id) if success => Some((
                    format!("the pool payout {id}"),
                    paid_line(id, tx_hash, block, eco_intent, across),
                )),
                TxPurpose::PoolPayout(id) => Some((
                    format!("the pool payout {id}"),
                    types::pool::PoolStep::PayoutReverted {
                        id,
                        block: Some(block),
                    },
                )),
                _ => None,
            };
            // a sweep closes as any transaction does; a revert is taken back only at the
            // crediting depth, where no reorg turns it into a run (`sweep_settled`). An Across
            // claim too: what it paid is credited when the canister's read of the pool chain
            // finds its transfer
            if let TxPurpose::Sweep(_) | TxPurpose::AcrossClaim(_) = entry.purpose {
                settle(entry, tx_hash, block, success, finals.credit, now);
                return 1;
            }
            if let Some((what, step)) = pool_close {
                let appended = match crate::storage::pool::record_at(step, now) {
                    Ok(_) => true,
                    Err(error) => {
                        log(&format!("{what}'s close was refused: {error}"));
                        false
                    }
                };
                if appended {
                    settle(entry, tx_hash, block, success, finals.credit, now);
                }
                return usize::from(appended);
            }
            // a flow's transaction closes with a flow line, whichever of the nonce's
            // transactions landed
            if let TxPurpose::Relay(quote_hash) | TxPurpose::Finish(quote_hash) = entry.purpose {
                let appended = append_event(EventType::Flow {
                    quote_hash,
                    step: types::FlowStep::TxClosed {
                        tx_hash,
                        block,
                        success,
                    },
                })
                .is_ok();
                if appended {
                    settle(entry, tx_hash, block, success, finals.credit, now);
                    // the closing line is appended once per transaction, so the fee is too
                    if success {
                        crate::books::credit_fee(chain_id, tx_hash, block, network_fee);
                    }
                }
                return usize::from(appended);
            }
            // a cancel closes with no line: `TxCancelled` already sealed that nonce's fate
            // before the bytes went out, so what the chain did with them changes nothing in
            // the fold
            settle(entry, tx_hash, block, success, finals.credit, now);
            1
        }
        // mined but not deep enough: nothing to do but wait
        Some(_) => 0,
        None => {
            push_again(entry, chain_id, now).await;
            0
        }
    }
}

/// The line a pool payout's successful transaction closes with: paid, with the Eco intent its
/// order published; or, for an order with an Across step (BSC design 6.2.7), paid with the one
/// deposit of the canister's its receipt shows, delivering until its fill is read. A receipt
/// that shows none of the canister's deposits, or two, names no deposit to follow: the payout
/// stops for a human, its USDC out on Across.
fn paid_line(
    id: u64,
    tx_hash: TxHash,
    block: BlockNumber,
    eco_intent: Option<[u8; 32]>,
    across: Option<crate::pool::across::DepositRead>,
) -> types::pool::PoolStep {
    use crate::pool::across::DepositRead;
    use types::pool::PoolStep;
    let across_order = read_state(|state| state.pool().lines().across_owed.contains(&id));
    match (across_order, across) {
        (false, _) => PoolStep::PayoutPaid {
            id,
            tx_hash,
            block,
            intent: eco_intent,
            across: None,
        },
        (true, Some(DepositRead::One(out))) => PoolStep::PayoutPaid {
            id,
            tx_hash,
            block,
            intent: None,
            across: Some(out),
        },
        (true, read) => PoolStep::PayoutStopped {
            id,
            why: format!(
                "ALERT: its Across order ran in {tx_hash}, and the receipt shows {} deposit of \
                 the canister's: its fill cannot be followed, for a human",
                match read {
                    Some(DepositRead::Many) => "more than one",
                    _ => "no",
                }
            ),
        },
    }
}

/// The open entries split into groups small enough that one outcall holds the answer.
///
/// An entry never straddles two calls, because the reader slices each entry's receipts out
/// of one answer in call order. An entry whose own hash list is longer than a chunk goes
/// alone and the cap is sized for it: the fee ceiling bounds how many replacements a nonce
/// can collect, so that list is short by construction.
fn receipt_chunks(open: Vec<OutboxEntry>) -> Vec<Vec<OutboxEntry>> {
    let mut chunks: Vec<Vec<OutboxEntry>> = Vec::new();
    let mut room = 0;
    for entry in open {
        let wanted = entry.hashes.len();
        match chunks.last_mut() {
            Some(chunk) if wanted <= room => {
                room -= wanted;
                chunk.push(entry);
            }
            _ => {
                room = RECEIPTS_PER_CALL.saturating_sub(wanted);
                chunks.push(vec![entry]);
            }
        }
    }
    chunks
}

/// What one chunk's answer may be: the head block, and a real receipt for every hash asked
/// for.
fn receipt_cap(receipts: usize) -> u64 {
    MAX_BLOCK_NUMBER_BYTES + MAX_RECEIPT_BYTES_PER_ITEM * receipts as u64
}

/// What one batch of broadcasts may answer: a hash or an error for each transaction in it.
fn send_cap(sends: usize) -> u64 {
    MAX_SEND_BYTES_PER_ITEM * sends as u64
}

/// How deep a receipt on `chain_id` must be before it closes an attempt: the swap depth,
/// 1 block on Ethereum too. Money arriving at the canister is credited
/// at [`credit_confirmations`].
///
/// A chain the config lists no depth for decides nothing. The alternative, a default of
/// one, is a depth nobody chose deciding money on a chain whose reorgs nobody sized, which
/// is how a deposit is claimed and then leaves the chain; a deploy that adds a chain adds
/// its depth with it.
pub(crate) fn confirmations(
    config: &types::Config,
    chain_id: ChainId,
) -> Result<BlockDepth, NoDepth> {
    if config.finalized.contains(chain_id) {
        return Err(NoDepth::Finalized { chain_id });
    }
    config
        .confirmations
        .get(&chain_id)
        .copied()
        .ok_or(NoDepth::Unset { chain_id })
}

/// The depth money arriving at the canister is credited at: the chain's crediting depth, or
/// its swap depth, and on Ethereum never below `MIN_ETHEREUM_CONFIRMATIONS`. A deposit, a
/// ck minter withdrawal's USDC and the books' balance read credit money a reorg would take
/// back, so they read this one and never the swap depth alone.
pub(crate) fn credit_confirmations(
    config: &types::Config,
    chain_id: ChainId,
) -> Result<BlockDepth, NoDepth> {
    if config.finalized.contains(chain_id) {
        return Err(NoDepth::Finalized { chain_id });
    }
    let depth = match config.credit_confirmations.get(chain_id) {
        Some(depth) => depth,
        None => confirmations(config, chain_id)?,
    };
    Ok(if chain_id == ChainId::ETHEREUM {
        depth.max(MIN_ETHEREUM_CONFIRMATIONS)
    } else {
        depth
    })
}

/// A sweep's transaction leaving the outbox at the crediting depth: one that reverted moved
/// nothing, so its amount goes back on its books. Earlier a one-block reorg could still turn
/// the revert into a run, and the books would hold money that left (rule A10).
fn sweep_settled(entry: &OutboxEntry, success: bool, now: Timestamp) {
    if let (TxPurpose::Sweep(id), false) = (entry.purpose, success) {
        crate::sweep::evm::taken_back(id, "its transaction reverted", now);
    }
}

/// Records the end of an entry's attempt on a receipt in `block` that `success`ed or not
/// (`TxFinal`), and what the outbox keeps of the entry: nothing once that receipt is final
/// under the crediting depth's `credit`, which on a chain that credits at its swap depth is
/// at once, as it always was. Otherwise the entry itself, marked closed with the block and the outcome: a reorg that
/// drops the transaction before the crediting depth finds its bytes to send again, and the
/// nonces behind it are not stranded.
///
/// A3: the mark is written on the entry as it is now, not as the pass read it before the
/// receipts' await, so a broadcast recorded meanwhile is kept. The removal needs no
/// re-read: the only writer that could have put another entry at this key is
/// `create_and_send`, which only ever writes at a number the allocator has just handed out,
/// and this one is not.
fn settle(
    entry: &OutboxEntry,
    tx_hash: TxHash,
    block: BlockNumber,
    success: bool,
    credit: Option<BlockNumber>,
    now: Timestamp,
) {
    let outcome = if success {
        types::events::TxOutcome::Succeeded { block }
    } else {
        types::events::TxOutcome::Reverted { block }
    };
    // the line the watcher's database files the transaction's end from; its close above is
    // what the fold acts on, so a line refused here is reported and nothing else
    if let Err(error) = crate::storage::events::append_event_at(
        EventType::TxFinal {
            purpose: entry.purpose,
            chain_id: entry.chain_id,
            nonce: entry.nonce,
            tx_hash,
            outcome,
        },
        now,
    ) {
        log(&format!(
            "the end of {tx_hash} on chain {} was not recorded: {error}",
            entry.chain_id
        ));
    }
    if is_final(block, credit) {
        sweep_settled(entry, success, now);
        outbox::remove(entry.key());
        return;
    }
    if let Some(mut current) = outbox::get(entry.key()) {
        current.closed_at = Some(block);
        current.closed_success = Some(success);
        outbox::put(current);
    }
}

/// The line a kept entry's receipt is reported with when its outcome is not the one the
/// attempt closed on, or nothing when it is.
fn divergence(entry: &OutboxEntry, receipt: &Receipt) -> Option<String> {
    let closed = entry.closed_success?;
    if closed == receipt.success {
        return None;
    }
    let outcome = |success: bool| if success { "succeeded" } else { "reverted" };
    Some(format!(
        "the transaction at chain {} nonce {} closed as {} at block {}, and its receipt now \
         reads {} at block {}: a reorg turned it round, and it is not decided again",
        entry.chain_id,
        entry.nonce,
        outcome(closed),
        entry.closed_at.map_or(0, BlockNumber::get),
        outcome(receipt.success),
        receipt.block,
    ))
}

/// One line in the canister's log. Unit tests run off the canister, where there is no log
/// to print to.
fn log(line: &str) {
    #[cfg(target_arch = "wasm32")]
    ic_cdk::println!("{line}");
    #[cfg(not(target_arch = "wasm32"))]
    let _ = line;
}

/// Where the canister's own Across deposits on a chain are read: the chain's SpokePool pin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AcrossPin {
    chain: ChainId,
    spoke_pool: EvmAddress,
    canister: EvmAddress,
}

/// What a receipt says.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Receipt {
    tx_hash: TxHash,
    block: BlockNumber,
    success: bool,
    /// What the order's fee step paid the canister in it.
    network_fee: types::TokenAmount,
    /// The Eco intent it published with the canister as the reward's creator: a pool payout's
    /// onward intent, whose refund comes back to the canister if no one fills it.
    eco_intent: Option<[u8; 32]>,
    /// The canister's own Across deposit in it, on a chain with Across's pins: a pool
    /// payout's, whose fill is read on its destination.
    across: Option<crate::pool::across::DepositRead>,
}

/// The hash of the one Eco intent Eco's Portal published in `receipt` whose reward's creator
/// is the canister, if it published one (the reader the rescue shares).
fn eco_intent_in(receipt: &Value, path: &crate::books::FeePath) -> Option<[u8; 32]> {
    crate::pool::eco::published_intent(
        &crate::pool::arrival::logs_of(receipt),
        path.eco_portal?,
        Some(path.canister),
    )
    .map(|intent| intent.intent_hash)
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
            network_fee: types::TokenAmount::ZERO,
            eco_intent: None,
            across: None,
        })
    };
    let receipt = read().ok_or(NotOurReceipt::Unmined)?;
    if !ours.contains(&receipt.tx_hash) {
        return Err(NotOurReceipt::AnotherTransaction);
    }
    Ok(receipt)
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
                bound_wei: max_transaction_cost(chain_id).into(),
            },
            TxError::NoGasBudget { chain_id, action } => Self::NoGasBudget {
                chain_id: chain_id.get(),
                action: format!("{action:?}"),
            },
            TxError::OverGasBudget {
                chain_id,
                action,
                cost,
                budget,
            } => Self::OverGasBudget {
                chain_id: chain_id.get(),
                action: format!("{action:?}"),
                cost_wei: cost.into(),
                budget_wei: budget.into(),
            },
            // the wire keeps its variants: the refusal's words carry the limit and the floor
            TxError::GasBelowIntrinsic {
                chain_id,
                gas_limit,
                least,
            } => Self::NoGasBudget {
                chain_id: chain_id.get(),
                action: format!(
                    "a transaction with a gas limit of {gas_limit}, below the {least} its data needs"
                ),
            },
            TxError::NoRecord(purpose) => Self::PurposeNeedsASwap {
                purpose: purpose.to_string(),
            },
            TxError::NonceCancelled {
                quote_hash,
                chain_id,
                nonce,
            } => Self::NonceCancelled {
                quote_hash: quote_hash.into_bytes(),
                chain_id: chain_id.get(),
                nonce: nonce.get(),
            },
            TxError::Append(error) => Self::Append(error.into()),
            TxError::Ecdsa(error) => Self::Ecdsa(error.into()),
        }
    }
}
