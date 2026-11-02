#[cfg(test)]
pub(crate) mod tests;

use crate::address::TextTooLong;
use crate::address::MAX_TEXT_BYTES;
use crate::books::BookStep;
use crate::chain::ChainId;
use crate::evm::{EvmAddress, EvmAddressError};
use crate::flow::FlowStep;
use crate::hash::{QuoteHash, TxHash};
use crate::numeric::{BlockNumber, EventIndex, GasAmount, Nonce, Timestamp, Wei, WeiPerGas};
use crate::pool::PoolStep;
use crate::sweep::Sweep;
use minicbor::{Decode, Encode};
use thiserror::Error;

/// What happened. Each variant's minicbor index is fixed once given; the gaps are variants
/// the canister no longer writes.
// a pool step that starts a payout carries its whole path (a route through ICP names three
// principals): a line is read and written one at a time, so its size is no cost worth a box
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub enum EventType {
    /// A controller wrote a new config. `json` is its public view as ops set it:
    /// compact JSON of the wire `Config`, candid field names in declaration order, maps in
    /// ascending chain id order, and every rpc url as `"***"`.
    #[n(0)]
    ConfigChanged {
        #[n(0)]
        json: String,
    },
    /// A user's deposit for a quote arrived.
    /// A controller handed out the service roles. Principals as text, so the audit line
    /// reads without a decoder.
    #[n(18)]
    RolesChanged {
        #[n(0)]
        quoter: String,
        #[n(1)]
        watcher: String,
    },
    /// A nonce was allocated to an outbound transaction, with every field that transaction
    /// will carry. Appended before the signature is asked for and before any other await,
    /// so the guard `nonce == next_nonce[chain_id]` is what makes two transactions unable
    /// to share a nonce (rule A4).
    #[n(20)]
    TxCreated {
        #[n(0)]
        purpose: TxPurpose,
        #[n(1)]
        chain_id: ChainId,
        #[n(2)]
        nonce: Nonce,
        #[n(3)]
        to: EvmAddress,
        #[n(4)]
        value: Wei,
        #[cbor(n(5), with = "minicbor::bytes")]
        data: Vec<u8>,
        #[n(6)]
        gas_limit: GasAmount,
        #[n(7)]
        max_fee: WeiPerGas,
        #[n(8)]
        max_priority_fee: WeiPerGas,
        /// The action whose gas budget the transaction is held to, it and every
        /// replacement of it. None on a line from before it was recorded.
        #[n(9)]
        action: Option<crate::config::GasAction>,
    },
    /// An open transaction was re-sent at the same nonce with a higher fee, because it was
    /// not landing (rule A5: an allocated nonce is never abandoned). Carries the bytes
    /// actually broadcast, so the log holds every transaction this canister ever put on a
    /// chain and not only the first of them.
    #[n(21)]
    TxReplaced {
        #[n(0)]
        purpose: TxPurpose,
        #[n(1)]
        chain_id: ChainId,
        #[n(2)]
        nonce: Nonce,
        #[n(3)]
        max_fee: WeiPerGas,
        #[n(4)]
        max_priority_fee: WeiPerGas,
        #[n(5)]
        tx_hash: TxHash,
        #[cbor(n(6), with = "minicbor::bytes")]
        raw_tx: Vec<u8>,
    },
    /// A nonce that was allocated and never signed for was spent by a cancel: a zero-value
    /// self-transfer at that number, recorded before it is broadcast the way every other
    /// transaction is (rule A6). It is the other way an allocation ends, so the pair of
    /// its signed record and this line is what makes rule A5 hold: every nonce `TxCreated` hands
    /// out reaches one of them, and the chain is never left with a gap it cannot mine past.
    #[n(22)]
    TxCancelled {
        #[n(0)]
        chain_id: ChainId,
        #[n(1)]
        nonce: Nonce,
        #[n(2)]
        tx_hash: TxHash,
        #[cbor(n(3), with = "minicbor::bytes")]
        raw_tx: Vec<u8>,
    },
    /// A step of one of the canister's flows: a gasless relay, ICP to EVM, or EVM to ICP
    /// (`types::flow`).
    #[n(24)]
    Flow {
        #[n(0)]
        quote_hash: QuoteHash,
        #[n(1)]
        step: FlowStep,
    },
    /// A step of a chain's USDC books (`types::books`).
    #[n(25)]
    Books {
        #[n(0)]
        chain_id: ChainId,
        #[n(1)]
        step: BookStep,
    },
    /// A step of the pool (`types::pool`).
    #[n(26)]
    Pool {
        #[n(0)]
        step: PoolStep,
    },
    /// A transaction the canister signed reached its end: mined at the depth the outbox
    /// waits for, or its nonce spent by a cancel.
    #[n(27)]
    TxFinal {
        #[n(0)]
        purpose: TxPurpose,
        #[n(1)]
        chain_id: ChainId,
        #[n(2)]
        nonce: Nonce,
        #[n(3)]
        tx_hash: TxHash,
        #[n(4)]
        outcome: TxOutcome,
    },
    /// A controller's sweep, its amount taken off the books it came from as it is signed
    /// or asked of the ledger.
    #[n(28)]
    Swept {
        #[n(0)]
        sweep: Sweep,
    },
    /// A sweep that moved nothing (reverted, cancelled, or refused by the ledger): its
    /// amount is back on the books it came from.
    #[n(29)]
    SweepReverted {
        #[n(0)]
        id: u64,
        #[n(1)]
        why: String,
    },
}

/// How a transaction the canister signed ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub enum TxOutcome {
    /// It ran, in `block`.
    #[n(0)]
    Succeeded {
        #[n(0)]
        block: BlockNumber,
    },
    /// It was mined in `block` and reverted.
    #[n(1)]
    Reverted {
        #[n(0)]
        block: BlockNumber,
    },
    /// Its nonce was spent by a cancel: it never ran.
    #[n(2)]
    Cancelled,
}

/// Why an outbound transaction exists. Every transaction this canister sends is one of
/// these, and all of them but [`TxPurpose::Cancel`] name the key they are held once under.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub enum TxPurpose {
    /// A same-nonce zero-value self-transfer, which abandons nothing: it spends a nonce
    /// whose transaction is no longer wanted (rule A5).
    #[n(5)]
    Cancel(#[n(0)] ChainId),
    /// A user's signed order relayed through Swapic (`runFor`), by its relay key.
    #[n(7)]
    Relay(#[n(0)] QuoteHash),
    /// A CCTP arrival finished by this canister, keyed by its message and action.
    #[n(9)]
    Finish(#[n(0)] QuoteHash),
    /// A pool payout from the canister's Base pocket (`Swapic.run`), by its payout id.
    #[n(10)]
    PoolPayout(#[n(0)] u64),
    /// An orphan's USDC sent back from the canister's Base address, by the hash of its
    /// credit key.
    #[n(11)]
    PoolTransfer(#[cbor(n(0), with = "minicbor::bytes")] [u8; 32]),
    /// The canister's standing USDC allowance to Swapic.
    #[n(12)]
    PoolApprove,
    /// A rebalance move's own transaction (its order's `run`, or a ck deposit), by the move's
    /// id: one out at a time per move.
    #[n(60)]
    PoolMove(#[n(0)] u64),
    /// Eco's `cancelAndProve` of an unfilled intent on its destination, by the order id that
    /// published it: one rescue per order.
    #[n(61)]
    EcoRescue(#[cbor(n(0), with = "minicbor::bytes")] [u8; 32]),
    /// A controller's sweep from the canister's address, by the sweep's id.
    #[n(62)]
    Sweep(#[n(0)] u64),
}

impl TxPurpose {
    /// The key this transaction is held once under, if it has one: a relay's or a finish's
    /// flow, and a tagged key for the pool's and a sweep's. A cancel has none.
    pub fn quote_hash(&self) -> Option<QuoteHash> {
        match self {
            Self::Relay(hash) | Self::Finish(hash) => Some(*hash),
            Self::PoolPayout(id) => Some(pool_key(b"swapic.pool.v1.tx.payout", &id.to_be_bytes())),
            Self::PoolTransfer(key) => Some(pool_key(b"swapic.pool.v1.tx.transfer", key)),
            Self::PoolApprove => Some(pool_key(b"swapic.pool.v1.tx.approve", &[])),
            Self::PoolMove(id) => Some(pool_key(b"swapic.pool.v1.tx.move", &id.to_be_bytes())),
            Self::EcoRescue(order_id) => Some(pool_key(b"swapic.eco.v1.tx.rescue", order_id)),
            Self::Sweep(id) => Some(pool_key(b"swapic.sweep.v1.tx", &id.to_be_bytes())),
            Self::Cancel(_) => None,
        }
    }
}

/// A pool transaction's key: `keccak256(tag ‖ key)`.
fn pool_key(tag: &[u8], key: &[u8]) -> QuoteHash {
    let mut preimage = Vec::with_capacity(tag.len() + key.len());
    preimage.extend_from_slice(tag);
    preimage.extend_from_slice(key);
    QuoteHash::new(alloy_primitives::keccak256(&preimage).0)
}

/// One entry of the log: where it sits, when it was written, and what happened.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub struct Event {
    #[n(0)]
    pub index: EventIndex,
    #[n(1)]
    pub timestamp: Timestamp,
    #[n(4)]
    pub payload: EventType,
}

/// Why a wire event is not a domain event, naming the field at fault.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum EventError {
    #[error("{field} is above u128::MAX")]
    AmountTooLarge { field: &'static str },
    #[error("{field} is {len} bytes, above the cap of {MAX_TEXT_BYTES}")]
    TextTooLong { field: &'static str, len: usize },
    #[error("{field} is not an address: {reason}")]
    NotAnAddress {
        field: &'static str,
        reason: EvmAddressError,
    },
    #[error("{field} is not a flow step")]
    NotAFlowStep { field: &'static str },
    #[error("{field} is not a book step")]
    NotABookStep { field: &'static str },
    #[error("{field} is not a gas action")]
    NotAGasAction { field: &'static str },
    #[error("{field} is not a pool step")]
    NotAPoolStep { field: &'static str },
    #[error("{field} has a subaccount that is not 32 bytes")]
    NotAnAccount { field: &'static str },
}

impl EventError {
    /// Names `field` in a text length failure.
    pub fn text_too_long(field: &'static str) -> impl FnOnce(TextTooLong) -> Self {
        move |TextTooLong { len }| Self::TextTooLong { field, len }
    }

    /// Names `field` in an amount that no canonical preimage holds.
    pub fn amount_too_large(field: &'static str) -> Self {
        Self::AmountTooLarge { field }
    }

    /// Names `field` in an address failure, carrying the reason the text is not one.
    pub fn not_an_address(field: &'static str) -> impl FnOnce(EvmAddressError) -> Self {
        move |reason| Self::NotAnAddress { field, reason }
    }
}

impl EventType {
    /// The signed transaction this line records the exact bytes of, if it records one:
    /// its chain, its hash and its bytes. What the watcher broadcasts.
    pub fn signed_tx(&self) -> Option<(ChainId, TxHash, &[u8])> {
        match self {
            EventType::TxReplaced {
                chain_id,
                tx_hash,
                raw_tx,
                ..
            }
            | EventType::TxCancelled {
                chain_id,
                tx_hash,
                raw_tx,
                ..
            } => Some((*chain_id, *tx_hash, raw_tx)),
            EventType::Flow {
                step:
                    FlowStep::GaslessRelayed {
                        chain_id,
                        tx_hash,
                        raw_tx,
                        ..
                    }
                    | FlowStep::ContinuationSigned {
                        chain_id,
                        tx_hash,
                        raw_tx,
                        ..
                    }
                    | FlowStep::CctpFinishSigned {
                        chain_id,
                        tx_hash,
                        raw_tx,
                        ..
                    },
                ..
            } => Some((*chain_id, *tx_hash, raw_tx)),
            EventType::Pool { step } => step
                .signed_tx()
                .map(|(chain_id, _, tx_hash, raw_tx)| (chain_id, tx_hash, raw_tx)),
            _ => None,
        }
    }

    /// Whether this line can give some payout or flow a next step the engine should act on
    /// now: the engine drives the moment such a line lands instead of waiting for its
    /// backstop. Exhaustive, so a new variant decides.
    pub fn moves_a_step(&self) -> bool {
        match self {
            EventType::ConfigChanged { .. }
            | EventType::RolesChanged { .. }
            | EventType::TxCreated { .. }
            | EventType::TxReplaced { .. }
            | EventType::TxCancelled { .. }
            | EventType::Books { .. }
            | EventType::TxFinal { .. }
            | EventType::Swept { .. }
            | EventType::SweepReverted { .. } => false,
            EventType::Flow { step, .. } => step.moves_a_step(),
            EventType::Pool { step } => step.moves_a_payout(),
        }
    }
}
crate::storable_as_cbor!(Event);
