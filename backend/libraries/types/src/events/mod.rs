#[cfg(test)]
mod tests;

use crate::address::{Address, TokenId, MAX_TEXT_BYTES};
use crate::canonical::CanonicalWriter;
use crate::chain::ChainId;
use crate::hash::{EventHash, QuoteHash, TxHash};
use crate::numeric::{Attempt, BlockNumber, EventIndex, Timestamp, TokenAmount};
use ic_stable_structures::storable::{Bound, Storable};
use minicbor::{Decode, Encode};
use sha2::Digest;
use std::borrow::{Borrow, Cow};
use thiserror::Error;

/// The number of [`EventType`] variants. The exhaustive match in
/// [`EventType::canonical_bytes`] is the compile-time check, the golden samples are the
/// coverage check.
pub const EVENT_VARIANT_COUNT: usize = 19;

/// What happened. Each variant's minicbor index is its canonical tag: assigned once, never
/// renumbered, never reused, only appended.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub enum EventType {
    /// A controller wrote a new config; `json` is its public view.
    #[n(0)]
    ConfigChanged {
        #[n(0)]
        json: String,
    },
    /// A user's deposit for a quote arrived.
    #[n(1)]
    FundsReceived {
        #[n(0)]
        quote_hash: QuoteHash,
        #[cbor(n(1), with = "minicbor::bytes")]
        quote_bytes: Vec<u8>,
        #[n(2)]
        chain_id: ChainId,
        #[n(3)]
        token: TokenId,
        #[n(4)]
        amount: TokenAmount,
        #[n(5)]
        tx_ref: String,
    },
    /// A transaction attempt was signed.
    #[n(2)]
    TxSigned {
        #[n(0)]
        quote_hash: QuoteHash,
        #[n(1)]
        attempt: Attempt,
        #[n(2)]
        chain_id: ChainId,
        #[n(3)]
        tx_hash: TxHash,
        #[cbor(n(4), with = "minicbor::bytes")]
        raw_tx: Vec<u8>,
    },
    /// The open attempt landed.
    #[n(3)]
    TxConfirmed {
        #[n(0)]
        quote_hash: QuoteHash,
        #[n(1)]
        attempt: Attempt,
        #[n(2)]
        chain_id: ChainId,
        #[n(3)]
        tx_hash: TxHash,
        #[n(4)]
        block: BlockNumber,
    },
    /// The open attempt failed.
    #[n(4)]
    TxFailed {
        #[n(0)]
        quote_hash: QuoteHash,
        #[n(1)]
        attempt: Attempt,
        #[n(2)]
        reason: String,
    },
    /// The source leg settled into the stable.
    #[n(5)]
    PaidInStable {
        #[n(0)]
        quote_hash: QuoteHash,
        #[n(1)]
        chain_id: ChainId,
        #[n(2)]
        amount: TokenAmount,
    },
    /// The swap paused on a question for the user.
    #[n(6)]
    DecisionRequired {
        #[n(0)]
        quote_hash: QuoteHash,
        #[n(1)]
        reason: String,
    },
    /// The user answered.
    #[n(7)]
    DecisionMade {
        #[n(0)]
        quote_hash: QuoteHash,
        #[n(1)]
        choice: Choice,
    },
    /// The swap turned into a refund.
    #[n(8)]
    RefundStarted {
        #[n(0)]
        quote_hash: QuoteHash,
        #[n(1)]
        reason: String,
    },
    /// The refund landed.
    #[n(9)]
    Refunded {
        #[n(0)]
        quote_hash: QuoteHash,
        #[n(1)]
        chain_id: ChainId,
        #[n(2)]
        token: TokenId,
        #[n(3)]
        amount: TokenAmount,
        #[n(4)]
        to: Address,
    },
    /// The swap delivered.
    #[n(10)]
    SwapDone {
        #[n(0)]
        quote_hash: QuoteHash,
    },
    /// The swap stopped for a human.
    #[n(11)]
    Frozen {
        #[n(0)]
        quote_hash: QuoteHash,
        #[n(1)]
        reason: String,
    },
    /// The platform took its fee.
    #[n(12)]
    FeeAccrued {
        #[n(0)]
        quote_hash: QuoteHash,
        #[n(1)]
        amount: TokenAmount,
    },
    /// Liquidity arrived in a chain's pocket.
    #[n(13)]
    PocketFunded {
        #[n(0)]
        chain_id: ChainId,
        #[n(1)]
        amount: TokenAmount,
    },
    /// Pocket liquidity was set aside for a swap.
    #[n(14)]
    PocketReserved {
        #[n(0)]
        quote_hash: QuoteHash,
        #[n(1)]
        chain_id: ChainId,
        #[n(2)]
        amount: TokenAmount,
    },
    /// Liquidity moved between two chains' pockets.
    #[n(15)]
    PocketRebalanced {
        #[n(0)]
        from_chain: ChainId,
        #[n(1)]
        to_chain: ChainId,
        #[n(2)]
        amount: TokenAmount,
        #[n(3)]
        route: String,
    },
    /// A reservation went back to the pocket.
    #[n(16)]
    PocketReleased {
        #[n(0)]
        quote_hash: QuoteHash,
        #[n(1)]
        chain_id: ChainId,
        #[n(2)]
        amount: TokenAmount,
    },
    /// A reservation left the pocket on-chain.
    #[n(17)]
    PocketSpent {
        #[n(0)]
        quote_hash: QuoteHash,
        #[n(1)]
        chain_id: ChainId,
        #[n(2)]
        amount: TokenAmount,
    },
    /// A controller handed out the service roles. Principals as text, so the audit line
    /// reads without a decoder.
    #[n(18)]
    RolesChanged {
        #[n(0)]
        quoter: String,
        #[n(1)]
        watcher: String,
    },
}

/// The user's answer to a paused swap. One byte in the preimage: Requote 0, Refund 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(index_only)]
pub enum Choice {
    #[n(0)]
    Requote,
    #[n(1)]
    Refund,
}

/// One entry of the log: the payload, where it sits, and its link in the hash chain.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub struct Event {
    #[n(0)]
    pub index: EventIndex,
    #[n(1)]
    pub timestamp: Timestamp,
    #[n(2)]
    pub parent_hash: EventHash,
    #[n(3)]
    pub hash: EventHash,
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
}

impl EventType {
    /// The canonical preimage: a u16 tag, then the variant's fields in declaration order,
    /// in the primitives of [`crate::canonical`]. `Choice` is one byte. Nothing here depends
    /// on minicbor or candid, so no storage or interface change can move a hash.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut w = CanonicalWriter::default();
        // exhaustive, no wildcard arm: a new variant fails to compile until it has a layout
        match self {
            EventType::ConfigChanged { json } => {
                w.put_u16(0).put_text(json);
            }
            EventType::FundsReceived {
                quote_hash,
                quote_bytes,
                chain_id,
                token,
                amount,
                tx_ref,
            } => {
                w.put_u16(1)
                    .put_hash(quote_hash.as_ref())
                    .put_bytes(quote_bytes)
                    .put_u64(chain_id.get())
                    .put_text(token.as_str())
                    .put_amount(*amount)
                    .put_text(tx_ref);
            }
            EventType::TxSigned {
                quote_hash,
                attempt,
                chain_id,
                tx_hash,
                raw_tx,
            } => {
                w.put_u16(2)
                    .put_hash(quote_hash.as_ref())
                    .put_u32(attempt.get())
                    .put_u64(chain_id.get())
                    .put_hash(tx_hash.as_ref())
                    .put_bytes(raw_tx);
            }
            EventType::TxConfirmed {
                quote_hash,
                attempt,
                chain_id,
                tx_hash,
                block,
            } => {
                w.put_u16(3)
                    .put_hash(quote_hash.as_ref())
                    .put_u32(attempt.get())
                    .put_u64(chain_id.get())
                    .put_hash(tx_hash.as_ref())
                    .put_u64(block.get());
            }
            EventType::TxFailed {
                quote_hash,
                attempt,
                reason,
            } => {
                w.put_u16(4)
                    .put_hash(quote_hash.as_ref())
                    .put_u32(attempt.get())
                    .put_text(reason);
            }
            EventType::PaidInStable {
                quote_hash,
                chain_id,
                amount,
            } => {
                w.put_u16(5)
                    .put_hash(quote_hash.as_ref())
                    .put_u64(chain_id.get())
                    .put_amount(*amount);
            }
            EventType::DecisionRequired { quote_hash, reason } => {
                w.put_u16(6).put_hash(quote_hash.as_ref()).put_text(reason);
            }
            EventType::DecisionMade { quote_hash, choice } => {
                w.put_u16(7)
                    .put_hash(quote_hash.as_ref())
                    .put_u8(match choice {
                        Choice::Requote => 0,
                        Choice::Refund => 1,
                    });
            }
            EventType::RefundStarted { quote_hash, reason } => {
                w.put_u16(8).put_hash(quote_hash.as_ref()).put_text(reason);
            }
            EventType::Refunded {
                quote_hash,
                chain_id,
                token,
                amount,
                to,
            } => {
                w.put_u16(9)
                    .put_hash(quote_hash.as_ref())
                    .put_u64(chain_id.get())
                    .put_text(token.as_str())
                    .put_amount(*amount)
                    .put_text(to.as_str());
            }
            EventType::SwapDone { quote_hash } => {
                w.put_u16(10).put_hash(quote_hash.as_ref());
            }
            EventType::Frozen { quote_hash, reason } => {
                w.put_u16(11).put_hash(quote_hash.as_ref()).put_text(reason);
            }
            EventType::FeeAccrued { quote_hash, amount } => {
                w.put_u16(12)
                    .put_hash(quote_hash.as_ref())
                    .put_amount(*amount);
            }
            EventType::PocketFunded { chain_id, amount } => {
                w.put_u16(13).put_u64(chain_id.get()).put_amount(*amount);
            }
            EventType::PocketReserved {
                quote_hash,
                chain_id,
                amount,
            } => {
                w.put_u16(14)
                    .put_hash(quote_hash.as_ref())
                    .put_u64(chain_id.get())
                    .put_amount(*amount);
            }
            EventType::PocketRebalanced {
                from_chain,
                to_chain,
                amount,
                route,
            } => {
                w.put_u16(15)
                    .put_u64(from_chain.get())
                    .put_u64(to_chain.get())
                    .put_amount(*amount)
                    .put_text(route);
            }
            EventType::PocketReleased {
                quote_hash,
                chain_id,
                amount,
            } => {
                w.put_u16(16)
                    .put_hash(quote_hash.as_ref())
                    .put_u64(chain_id.get())
                    .put_amount(*amount);
            }
            EventType::PocketSpent {
                quote_hash,
                chain_id,
                amount,
            } => {
                w.put_u16(17)
                    .put_hash(quote_hash.as_ref())
                    .put_u64(chain_id.get())
                    .put_amount(*amount);
            }
            EventType::RolesChanged { quoter, watcher } => {
                w.put_u16(18).put_text(quoter).put_text(watcher);
            }
        }
        w.into_bytes()
    }
}

/// The link hash: sha256 over `index u64 | timestamp u64 | parent_hash | canonical bytes`.
pub fn event_hash(
    index: EventIndex,
    timestamp: Timestamp,
    parent_hash: &EventHash,
    payload: &EventType,
) -> EventHash {
    let mut h = sha2::Sha256::new();
    h.update(index.get().to_be_bytes());
    h.update(timestamp.as_nanos().to_be_bytes());
    h.update(parent_hash.as_ref());
    h.update(payload.canonical_bytes());
    EventHash::new(h.finalize().into())
}

impl Event {
    /// The event at `index` on top of `parent_hash`, with its link hash.
    pub fn seal(
        index: EventIndex,
        timestamp: Timestamp,
        parent_hash: EventHash,
        payload: EventType,
    ) -> Event {
        Event {
            index,
            timestamp,
            parent_hash,
            hash: event_hash(index, timestamp, &parent_hash, &payload),
            payload,
        }
    }
}

/// Genesis-anchored: index 0 links to [`EventHash::ZERO`] and every link after it holds.
/// Takes anything iterable, by value or by reference, so a stable log streams through the
/// same rule a slice does.
pub fn chain_is_valid<E: Borrow<Event>>(events: impl IntoIterator<Item = E>) -> bool {
    let mut parent_hash = EventHash::ZERO;
    for (index, event) in (0..).map(EventIndex::new).zip(events) {
        let event = event.borrow();
        if event.index != index || event.parent_hash != parent_hash {
            return false;
        }
        if event.hash
            != event_hash(
                event.index,
                event.timestamp,
                &event.parent_hash,
                &event.payload,
            )
        {
            return false;
        }
        parent_hash = event.hash;
    }
    true
}

/// Stored as minicbor. The hash chain never reads these bytes, so a storage change cannot
/// move a hash; a stored event that no longer decodes traps, which leaves an upgrade on
/// the wasm that wrote it.
impl Storable for Event {
    fn to_bytes(&self) -> Cow<'_, [u8]> {
        Cow::Owned(minicbor::to_vec(self).expect("BUG: encoding an event into a Vec is infallible"))
    }

    fn from_bytes(bytes: Cow<[u8]>) -> Self {
        minicbor::decode(&bytes)
            .unwrap_or_else(|e| panic!("failed to decode event {}: {e}", hex::encode(&bytes)))
    }

    const BOUND: Bound = Bound::Unbounded;
}
