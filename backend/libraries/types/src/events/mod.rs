#[cfg(test)]
pub(crate) mod tests;

use crate::address::{Address, TextTooLong, TokenId, MAX_TEXT_BYTES};
use crate::canonical::{CanonicalError, CanonicalWriter};
use crate::chain::ChainId;
use crate::evm::{EvmAddress, EvmAddressError};
use crate::hash::{EventHash, QuoteHash, TxHash};
use crate::numeric::{
    Attempt, BlockNumber, EventIndex, GasAmount, Nonce, Timestamp, TokenAmount, Wei, WeiPerGas,
};
use minicbor::{Decode, Encode};
use sha2::Digest;
use std::borrow::Borrow;
use thiserror::Error;

/// The number of [`EventType`] variants. The exhaustive match in
/// [`EventType::canonical_bytes`] is the compile-time check, the golden samples are the
/// coverage check.
pub const EVENT_VARIANT_COUNT: usize = 24;

/// What happened. Each variant's minicbor index is its canonical tag: assigned once, never
/// renumbered, never reused, only appended.
///
/// Stored as minicbor: `#[n]` indices are append-only, never renumbered or reused, and a
/// new field is optional.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub enum EventType {
    /// A controller wrote a new config. `json` is its public view as operators set it:
    /// compact JSON of the wire `Config`, candid field names in declaration order, maps in
    /// ascending chain id order, `max_swap_usd` as a decimal string, and every rpc url as
    /// `"***"`. It is hashed into the chain, so the shape never changes.
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
    /// A swap's waiting index entries were made to agree with the swap: what the fold could
    /// not have produced was dropped, and the wait the swap is in is there. Only a
    /// divergence needs one, so this line is the repair's explanation: without it the fold
    /// would stop being the fold of the log.
    #[n(19)]
    WaitingRepaired {
        #[n(0)]
        quote_hash: QuoteHash,
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
    /// `TxSigned` and this line is what makes rule A5 hold: every nonce `TxCreated` hands
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
    /// A gasless pull was signed: the transaction that takes a user's funds into the vault
    /// with the permit they signed, recorded with its exact bytes before it is broadcast
    /// (rule A6). It names the quote and the nonce rather than a swap's attempt, because no
    /// swap exists yet: the deposit the pull makes is what `claim_swap` then verifies on
    /// the chain, and that is what creates the swap. It spends the nonce `TxCreated` handed
    /// out for the pull, the way `TxSigned` spends a swap's.
    #[n(23)]
    PullSigned {
        #[n(0)]
        quote_hash: QuoteHash,
        #[n(1)]
        chain_id: ChainId,
        #[n(2)]
        nonce: Nonce,
        #[n(3)]
        tx_hash: TxHash,
        #[cbor(n(4), with = "minicbor::bytes")]
        raw_tx: Vec<u8>,
    },
}

/// Why an outbound transaction exists. Every transaction this canister sends is one of
/// these, and all but the last name the swap they belong to.
///
/// Stored as minicbor: `#[n]` indices are append-only, never renumbered or reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub enum TxPurpose {
    /// The source leg: value leaves for the rail.
    #[n(0)]
    Burn(#[n(0)] QuoteHash),
    /// The destination leg: the rail's value arrives.
    #[n(1)]
    Mint(#[n(0)] QuoteHash),
    /// The user is paid.
    #[n(2)]
    Payout(#[n(0)] QuoteHash),
    /// The user is paid back.
    #[n(3)]
    Refund(#[n(0)] QuoteHash),
    /// A permit is used to pull the user's funds without their gas.
    #[n(4)]
    GaslessPull(#[n(0)] QuoteHash),
    /// A same-nonce zero-value self-transfer, which abandons nothing: it spends a nonce
    /// whose transaction is no longer wanted (rule A5).
    #[n(5)]
    Cancel(#[n(0)] ChainId),
    /// The rail gives the funds back to the source vault: an intent nobody filled is
    /// refunded to the vault, so the user can be refunded from it.
    #[n(6)]
    Reclaim(#[n(0)] QuoteHash),
}

impl TxPurpose {
    /// The quote this transaction is for, if it is for one: the swap's id for the legs of a
    /// swap, and the quote a pull is paying for before its swap exists.
    pub fn quote_hash(&self) -> Option<QuoteHash> {
        match self {
            Self::Burn(hash)
            | Self::Mint(hash)
            | Self::Payout(hash)
            | Self::Refund(hash)
            | Self::GaslessPull(hash)
            | Self::Reclaim(hash) => Some(*hash),
            Self::Cancel(_) => None,
        }
    }

    /// The swap whose attempt this transaction is, if it is one: a pull runs before any
    /// swap exists and a cancel belongs to none, so neither is signed against an attempt.
    pub fn attempt_of(&self) -> Option<QuoteHash> {
        match self {
            Self::Burn(hash)
            | Self::Mint(hash)
            | Self::Payout(hash)
            | Self::Refund(hash)
            | Self::Reclaim(hash) => Some(*hash),
            Self::GaslessPull(_) | Self::Cancel(_) => None,
        }
    }

    /// One byte and then its argument: a swap id for the six that name one, the chain id
    /// for the cancel. The tag makes the two shapes unambiguous, and a tag is never
    /// renumbered or reused.
    fn write_canonical(&self, w: &mut CanonicalWriter) {
        match self {
            Self::Burn(hash) => w.put_u8(0).put_hash(hash.as_ref()),
            Self::Mint(hash) => w.put_u8(1).put_hash(hash.as_ref()),
            Self::Payout(hash) => w.put_u8(2).put_hash(hash.as_ref()),
            Self::Refund(hash) => w.put_u8(3).put_hash(hash.as_ref()),
            Self::GaslessPull(hash) => w.put_u8(4).put_hash(hash.as_ref()),
            Self::Cancel(chain_id) => w.put_u8(5).put_u64(chain_id.get()),
            Self::Reclaim(hash) => w.put_u8(6).put_hash(hash.as_ref()),
        };
    }
}

/// The user's answer to a paused swap. One byte in the preimage: Requote 0, Refund 1.
///
/// Stored as minicbor: `#[n]` indices are append-only, never renumbered or reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(index_only)]
pub enum Choice {
    #[n(0)]
    Requote,
    #[n(1)]
    Refund,
}

/// One entry of the log: the payload, where it sits, and its link in the hash chain.
///
/// Stored as minicbor: `#[n]` indices are append-only, never renumbered or reused, and a
/// new field is optional.
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
    #[error("{field} is not an address: {reason}")]
    NotAnAddress {
        field: &'static str,
        reason: EvmAddressError,
    },
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
    /// The token amount the event carries, if it carries one. Exhaustive, so a new variant
    /// decides whether it moves value.
    pub fn amount(&self) -> Option<TokenAmount> {
        match self {
            EventType::FundsReceived { amount, .. }
            | EventType::PaidInStable { amount, .. }
            | EventType::Refunded { amount, .. }
            | EventType::FeeAccrued { amount, .. }
            | EventType::PocketFunded { amount, .. }
            | EventType::PocketReserved { amount, .. }
            | EventType::PocketRebalanced { amount, .. }
            | EventType::PocketReleased { amount, .. }
            | EventType::PocketSpent { amount, .. } => Some(*amount),
            EventType::ConfigChanged { .. }
            | EventType::TxSigned { .. }
            | EventType::TxConfirmed { .. }
            | EventType::TxFailed { .. }
            | EventType::DecisionRequired { .. }
            | EventType::DecisionMade { .. }
            | EventType::RefundStarted { .. }
            | EventType::SwapDone { .. }
            | EventType::Frozen { .. }
            | EventType::RolesChanged { .. }
            | EventType::WaitingRepaired { .. }
            // a transaction's value, gas and fees are not token amounts: `State::check`
            // holds them to the same 16-byte range the preimage writes them in
            | EventType::TxCreated { .. }
            | EventType::TxReplaced { .. }
            | EventType::TxCancelled { .. }
            | EventType::PullSigned { .. } => None,
        }
    }

    /// The canonical preimage: a u16 tag, then the variant's fields in declaration order,
    /// in the primitives of [`crate::canonical`]. `Choice` is one byte. Nothing here depends
    /// on minicbor or candid, so no storage or interface change can move a hash. An amount
    /// above `u128::MAX` has no preimage.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, CanonicalError> {
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
            EventType::WaitingRepaired { quote_hash } => {
                w.put_u16(19).put_hash(quote_hash.as_ref());
            }
            EventType::TxCreated {
                purpose,
                chain_id,
                nonce,
                to,
                value,
                data,
                gas_limit,
                max_fee,
                max_priority_fee,
            } => {
                w.put_u16(20);
                purpose.write_canonical(&mut w);
                w.put_u64(chain_id.get())
                    .put_u64(nonce.get())
                    .put_address(to.as_bytes())
                    .put_amount(*value)
                    .put_bytes(data)
                    .put_amount(*gas_limit)
                    .put_amount(*max_fee)
                    .put_amount(*max_priority_fee);
            }
            EventType::TxReplaced {
                purpose,
                chain_id,
                nonce,
                max_fee,
                max_priority_fee,
                tx_hash,
                raw_tx,
            } => {
                w.put_u16(21);
                purpose.write_canonical(&mut w);
                w.put_u64(chain_id.get())
                    .put_u64(nonce.get())
                    .put_amount(*max_fee)
                    .put_amount(*max_priority_fee)
                    .put_hash(tx_hash.as_ref())
                    .put_bytes(raw_tx);
            }
            EventType::TxCancelled {
                chain_id,
                nonce,
                tx_hash,
                raw_tx,
            } => {
                w.put_u16(22)
                    .put_u64(chain_id.get())
                    .put_u64(nonce.get())
                    .put_hash(tx_hash.as_ref())
                    .put_bytes(raw_tx);
            }
            EventType::PullSigned {
                quote_hash,
                chain_id,
                nonce,
                tx_hash,
                raw_tx,
            } => {
                w.put_u16(23)
                    .put_hash(quote_hash.as_ref())
                    .put_u64(chain_id.get())
                    .put_u64(nonce.get())
                    .put_hash(tx_hash.as_ref())
                    .put_bytes(raw_tx);
            }
        }
        w.finish()
    }
}

/// The link hash: sha256 over `index u64 | timestamp u64 | parent_hash | canonical bytes`.
pub fn event_hash(
    index: EventIndex,
    timestamp: Timestamp,
    parent_hash: &EventHash,
    payload: &EventType,
) -> Result<EventHash, CanonicalError> {
    let mut h = sha2::Sha256::new();
    h.update(index.get().to_be_bytes());
    h.update(timestamp.as_nanos().to_be_bytes());
    h.update(parent_hash.as_ref());
    h.update(payload.canonical_bytes()?);
    Ok(EventHash::new(h.finalize().into()))
}

impl Event {
    /// The event at `index` on top of `parent_hash`, with its link hash, or the reason the
    /// payload has no preimage to hash.
    pub fn seal(
        index: EventIndex,
        timestamp: Timestamp,
        parent_hash: EventHash,
        payload: EventType,
    ) -> Result<Event, CanonicalError> {
        Ok(Event {
            index,
            timestamp,
            parent_hash,
            hash: event_hash(index, timestamp, &parent_hash, &payload)?,
            payload,
        })
    }
}

/// Why an entry is not the link the chain expected where it sits.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum LinkError {
    #[error("event {found} sits where event {expected} belongs")]
    OutOfSequence {
        expected: EventIndex,
        found: EventIndex,
    },
    #[error("event {index} links to {parent} but the entry before it sealed {head}")]
    Unlinked {
        index: EventIndex,
        parent: EventHash,
        head: EventHash,
    },
    #[error("event {index} does not hash to the {hash} it carries")]
    HashMismatch { index: EventIndex, hash: EventHash },
}

/// One link of the chain: `event` must sit at `index`, link to `parent`, and hash to the
/// hash it carries. Answers the hash the next link is checked against, so a verification
/// can stop anywhere and resume from what it answered. An event with no preimage has no
/// hash to compare with, which is a mismatch like any other.
pub fn check_link(
    event: &Event,
    index: EventIndex,
    parent: EventHash,
) -> Result<EventHash, LinkError> {
    if event.index != index {
        return Err(LinkError::OutOfSequence {
            expected: index,
            found: event.index,
        });
    }
    if event.parent_hash != parent {
        return Err(LinkError::Unlinked {
            index,
            parent: event.parent_hash,
            head: parent,
        });
    }
    if event_hash(
        event.index,
        event.timestamp,
        &event.parent_hash,
        &event.payload,
    ) != Ok(event.hash)
    {
        return Err(LinkError::HashMismatch {
            index,
            hash: event.hash,
        });
    }
    Ok(event.hash)
}

/// Genesis-anchored: index 0 links to [`EventHash::ZERO`] and every link after it holds.
/// Takes anything iterable, by value or by reference, so a stable log streams through the
/// same rule a slice does. An event with no preimage is not a valid link.
pub fn chain_is_valid<E: Borrow<Event>>(events: impl IntoIterator<Item = E>) -> bool {
    let mut parent_hash = EventHash::ZERO;
    for (index, event) in (0..).map(EventIndex::new).zip(events) {
        match check_link(event.borrow(), index, parent_hash) {
            Ok(hash) => parent_hash = hash,
            Err(_) => return false,
        }
    }
    true
}

// Stored as minicbor. The hash chain never reads these bytes, so a storage change cannot
// move a hash.
crate::storable_as_cbor!(Event);
