#[cfg(test)]
mod tests;

use crate::address::TokenId;
use crate::chain::ChainId;
use crate::events::TxPurpose;
use crate::hash::{QuoteHash, TxHash};
use crate::numeric::{Attempt, Nonce, Timestamp, TokenAmount};
use crate::quote::{Quote, QuoteError};
use ic_stable_structures::storable::{Bound, Storable};
use minicbor::{Decode, Encode};
use std::borrow::Cow;
use thiserror::Error;

/// Where a swap is in its lifecycle.
///
/// Stored as minicbor: `#[n]` indices are append-only, never renumbered or reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(index_only)]
pub enum SwapStatus {
    #[n(0)]
    FundsReceived,
    #[n(1)]
    Executing,
    #[n(2)]
    PaidInStable,
    #[n(3)]
    Delivering,
    #[n(4)]
    WaitingForUser,
    #[n(5)]
    Done,
    #[n(6)]
    Refunding,
    #[n(7)]
    Refunded,
    #[n(8)]
    Frozen,
}

impl SwapStatus {
    /// Terminal: no further work is ever scheduled for the swap.
    pub fn is_closed(self) -> bool {
        matches!(self, Self::Done | Self::Refunded | Self::Frozen)
    }
}

/// The leg of a swap a transaction attempt was signed for: what the engine reads to know
/// where a swap is between its lines, since the status alone does not say whether the
/// attempt that just closed was the burn or the mint.
///
/// Stored as minicbor: `#[n]` indices are append-only, never renumbered or reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(index_only)]
pub enum Leg {
    /// The user's funds leave the source vault for the rail.
    #[n(0)]
    Burn,
    /// The rail's funds arrive in the destination vault.
    #[n(1)]
    Mint,
    /// The user is paid from the destination vault.
    #[n(2)]
    Payout,
    /// The user is paid back from the source vault.
    #[n(3)]
    Refund,
    /// The rail gives the funds back to the source vault, for a refund to follow.
    #[n(4)]
    Reclaim,
}

impl Leg {
    /// The leg a transaction of `purpose` is, for the purposes that are a swap's attempt.
    pub fn of(purpose: TxPurpose) -> Option<Self> {
        match purpose {
            TxPurpose::Burn(_) => Some(Self::Burn),
            TxPurpose::Mint(_) => Some(Self::Mint),
            TxPurpose::Payout(_) => Some(Self::Payout),
            TxPurpose::Refund(_) => Some(Self::Refund),
            TxPurpose::Reclaim(_) => Some(Self::Reclaim),
            TxPurpose::GaslessPull(_) | TxPurpose::Cancel(_) => None,
        }
    }
}

/// How a swap's latest attempt ended, once it has.
///
/// Stored as minicbor: `#[n]` indices are append-only, never renumbered or reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(index_only)]
pub enum Outcome {
    #[n(0)]
    Confirmed,
    #[n(1)]
    Failed,
}

/// The folded state of one swap.
///
/// Stored as minicbor: `#[n]` indices are append-only, never renumbered or reused, and a
/// new field is optional.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub struct Swap {
    /// The canonical preimage of the quote the funds arrived for.
    #[cbor(n(0), with = "minicbor::bytes")]
    pub quote_bytes: Vec<u8>,
    #[n(1)]
    pub status: SwapStatus,
    /// The latest attempt signed, open or not.
    #[n(2)]
    pub last_attempt: Option<Attempt>,
    /// The attempt signed and not yet confirmed or failed.
    #[n(3)]
    pub open_attempt: Option<Attempt>,
    #[n(4)]
    pub src_chain: ChainId,
    #[n(5)]
    pub src_token: TokenId,
    #[n(6)]
    pub amount_in: TokenAmount,
    /// `None` until the swap is paid in stable, so a payment of zero still counts as paid.
    #[n(7)]
    pub amount_paid: Option<TokenAmount>,
    /// When the swap paused on the user, while it waits.
    #[n(8)]
    pub waiting_since: Option<Timestamp>,
    /// The leg the latest attempt was signed for, once one has been.
    #[n(9)]
    pub last_leg: Option<Leg>,
    /// How the latest attempt ended: absent while it is open, or before any was signed.
    #[n(10)]
    pub last_outcome: Option<Outcome>,
    /// The transaction the latest attempt confirmed as, once it has: what binds a pushed
    /// attestation to the swap's own burn, and what the mint read is looked up by. Absent
    /// while the attempt is open, after one that failed, and before any was signed.
    #[n(11)]
    pub last_tx_hash: Option<TxHash>,
    /// What the payout leg was signed to pay the user, read off the calldata this canister
    /// built for it. The record of a delivered swap is made from this and never from the
    /// live config, so a fee the operator moved between the send and the confirmation
    /// cannot change what the log says was paid.
    #[n(12)]
    pub paid_out: Option<TokenAmount>,
    /// The platform's fee on this swap, once the log has accrued it: what `record_done`
    /// reads, so a swap recorded again after a refused `SwapDone` does not accrue its fee
    /// twice. Absent while nothing was accrued, so a swap written before it existed reads
    /// back without one.
    #[n(13)]
    pub fee_accrued: Option<TokenAmount>,
}

/// Why an event cannot move the state it was offered to.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum TransitionError {
    #[error("swap {0} already has funds")]
    SwapExists(QuoteHash),
    #[error("unknown swap {0}")]
    UnknownSwap(QuoteHash),
    #[error("no pocket on chain {0}")]
    UnknownPocket(ChainId),
    #[error("swap is closed ({0:?})")]
    SwapClosed(SwapStatus),
    #[error("swap is waiting for the user")]
    WaitingForUser,
    #[error("swap is not waiting for the user ({0:?})")]
    NotWaitingForUser(SwapStatus),
    #[error("attempt {0} is still open")]
    AttemptStillOpen(Attempt),
    #[error("attempt {attempt} out of sequence, expected {expected:?}")]
    AttemptOutOfSequence {
        attempt: Attempt,
        expected: Option<Attempt>,
    },
    #[error("attempt {0} is not open")]
    AttemptNotOpen(Attempt),
    #[error("swap {0} has no open attempt to replace")]
    NoOpenAttempt(QuoteHash),
    #[error("nonce {nonce} on chain {chain_id} is not the {expected} the allocator is at")]
    NonceOutOfSequence {
        chain_id: ChainId,
        nonce: Nonce,
        expected: Nonce,
    },
    #[error("nonce {nonce} on chain {chain_id} was never allocated: the allocator is at {next}")]
    NonceNeverAllocated {
        chain_id: ChainId,
        nonce: Nonce,
        next: Nonce,
    },
    #[error("the allocator on chain {chain_id} has no number left to hand out")]
    NonceExhausted { chain_id: ChainId },
    #[error("swap {0} already holds a nonce it has not signed for")]
    NonceStillUnsigned(QuoteHash),
    #[error("nonce {nonce} on chain {chain_id} is not one that is waiting for a signature")]
    NonceNotUnsigned { chain_id: ChainId, nonce: Nonce },
    #[error(
        "swap {quote_hash} holds no nonce on chain {chain_id} waiting for its signature: the \
         allocation was cancelled, or never made"
    )]
    NoUnsignedNonce {
        quote_hash: QuoteHash,
        chain_id: ChainId,
    },
    #[error(
        "nonce {nonce} on chain {chain_id} is not one waiting for the pull of quote \
         {quote_hash} to be signed"
    )]
    NonceNotHeldForPull {
        chain_id: ChainId,
        nonce: Nonce,
        quote_hash: QuoteHash,
    },
    #[error("swap is not executing ({0:?})")]
    NotExecuting(SwapStatus),
    #[error("swap is already paid in stable")]
    AlreadyPaid,
    #[error("swap cannot start a refund ({0:?})")]
    CannotStartRefund(SwapStatus),
    #[error("swap is refunding, and a refund is never turned back into a delivery")]
    CannotAskWhileRefunding,
    #[error("swap is not refunding ({0:?})")]
    NotRefunding(SwapStatus),
    #[error("swap is not in flight ({0:?})")]
    NotInFlight(SwapStatus),
    #[error("accrued fees would overflow")]
    FeesOverflow,
    #[error("amount {0} is above u128::MAX, which no event can carry")]
    AmountOutOfRange(TokenAmount),
    #[error("the funds arrived on chain {logged}, and the quote is for chain {quoted}")]
    FundsChainNotTheQuotes { logged: ChainId, quoted: ChainId },
    #[error("the funds that arrived are {logged}, and the quote is for {quoted}")]
    FundsTokenNotTheQuotes { logged: TokenId, quoted: TokenId },
    #[error("{logged} arrived, and the quote is for {quoted}")]
    FundsAmountNotTheQuotes {
        logged: TokenAmount,
        quoted: TokenAmount,
    },
    #[error("quote_bytes are not a quote this canister reads: {0}")]
    UnparseableQuote(#[from] QuoteError),
    #[error("quote_bytes hash to {computed}, and the event carries {declared}")]
    QuoteHashMismatch {
        declared: QuoteHash,
        computed: QuoteHash,
    },
    #[error(transparent)]
    Pocket(#[from] PocketError),
}

impl Swap {
    /// The wait the auto-refund index holds for this swap, if it holds one: the instant the
    /// swap paused on its user, when it is waiting, its quote asks for an automatic refund,
    /// and the clock is running. The index is the queue the expiry timer works through, so
    /// a swap that waits for a human is not in it, and neither is one whose bytes an older
    /// wasm recorded and this one does not read.
    pub fn auto_refund_wait(&self) -> Option<Timestamp> {
        if self.status != SwapStatus::WaitingForUser {
            return None;
        }
        let auto_refund = Quote::parse(&self.quote_bytes).is_ok_and(|quote| quote.auto_refund);
        self.waiting_since.filter(|_| auto_refund)
    }

    /// The number the next attempt must carry: one, then one past the last.
    pub fn next_attempt(&self) -> Option<Attempt> {
        self.last_attempt
            .map_or(Some(Attempt::FIRST), Attempt::next)
    }

    pub fn ensure_not_closed(&self) -> Result<(), TransitionError> {
        if self.status.is_closed() {
            return Err(TransitionError::SwapClosed(self.status));
        }
        Ok(())
    }

    pub fn ensure_not_waiting(&self) -> Result<(), TransitionError> {
        if self.status == SwapStatus::WaitingForUser {
            return Err(TransitionError::WaitingForUser);
        }
        Ok(())
    }

    pub fn ensure_waiting(&self) -> Result<(), TransitionError> {
        if self.status != SwapStatus::WaitingForUser {
            return Err(TransitionError::NotWaitingForUser(self.status));
        }
        Ok(())
    }

    pub fn ensure_no_open_attempt(&self) -> Result<(), TransitionError> {
        match self.open_attempt {
            Some(open) => Err(TransitionError::AttemptStillOpen(open)),
            None => Ok(()),
        }
    }

    /// Attempts are numbered without gaps.
    pub fn ensure_next_attempt(&self, attempt: Attempt) -> Result<(), TransitionError> {
        let expected = self.next_attempt();
        if expected != Some(attempt) {
            return Err(TransitionError::AttemptOutOfSequence { attempt, expected });
        }
        Ok(())
    }

    pub fn ensure_attempt_open(&self, attempt: Attempt) -> Result<(), TransitionError> {
        if self.open_attempt != Some(attempt) {
            return Err(TransitionError::AttemptNotOpen(attempt));
        }
        Ok(())
    }

    pub fn ensure_executing(&self) -> Result<(), TransitionError> {
        match self.status {
            SwapStatus::Executing | SwapStatus::FundsReceived => Ok(()),
            status => Err(TransitionError::NotExecuting(status)),
        }
    }

    /// A swap is paid in stable at most once, so a requote cannot overwrite the amount,
    /// whatever the amount was.
    pub fn ensure_unpaid(&self) -> Result<(), TransitionError> {
        if self.amount_paid.is_some() {
            return Err(TransitionError::AlreadyPaid);
        }
        Ok(())
    }

    pub fn ensure_can_start_refund(&self) -> Result<(), TransitionError> {
        if self.status.is_closed() || self.status == SwapStatus::Refunding {
            return Err(TransitionError::CannotStartRefund(self.status));
        }
        Ok(())
    }

    /// A refund is one way: once the money is on its way back, no question put to the user
    /// can turn it into a delivery. A refund attempt that failed is retried as another
    /// refund attempt, which needs no question.
    pub fn ensure_not_refunding(&self) -> Result<(), TransitionError> {
        if self.status == SwapStatus::Refunding {
            return Err(TransitionError::CannotAskWhileRefunding);
        }
        Ok(())
    }

    pub fn ensure_refunding(&self) -> Result<(), TransitionError> {
        if self.status != SwapStatus::Refunding {
            return Err(TransitionError::NotRefunding(self.status));
        }
        Ok(())
    }

    pub fn ensure_in_flight(&self) -> Result<(), TransitionError> {
        match self.status {
            SwapStatus::Delivering | SwapStatus::Executing | SwapStatus::PaidInStable => Ok(()),
            status => Err(TransitionError::NotInFlight(status)),
        }
    }
}

/// A swap waiting for its user, as the waiting index keys it: by when the wait began, then
/// by swap id, so a walk from the first key meets the longest wait first.
///
/// The index is stored as these keys' forty bytes below; the deep audit's saved fold holds
/// them as minicbor, where `#[n]` indices are append-only, never renumbered or reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Encode, Decode)]
pub struct WaitingKey {
    #[n(0)]
    pub since: Timestamp,
    #[n(1)]
    pub quote_hash: QuoteHash,
}

/// Forty bytes: the eight big-endian bytes of `since`, then the hash, so byte order and key
/// order agree.
impl Storable for WaitingKey {
    fn to_bytes(&self) -> Cow<'_, [u8]> {
        let mut bytes = Vec::with_capacity(40);
        bytes.extend_from_slice(&self.since.as_nanos().to_be_bytes());
        bytes.extend_from_slice(self.quote_hash.as_ref());
        Cow::Owned(bytes)
    }

    fn from_bytes(bytes: Cow<[u8]>) -> Self {
        let (since, quote_hash) = bytes
            .split_first_chunk::<8>()
            .expect("BUG: a stored waiting key is written as exactly 40 bytes");
        Self {
            since: Timestamp::from_nanos(u64::from_be_bytes(*since)),
            quote_hash: QuoteHash::new(
                quote_hash
                    .try_into()
                    .expect("BUG: a stored waiting key is written as exactly 40 bytes"),
            ),
        }
    }

    const BOUND: Bound = Bound::Bounded {
        max_size: 40,
        is_fixed_size: true,
    };
}

/// One chain's liquidity.
///
/// Stored as minicbor: `#[n]` indices are append-only, never renumbered or reused, and a
/// new field is optional.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Encode, Decode)]
pub struct Pocket {
    /// Free to reserve or rebalance.
    #[n(0)]
    pub available: TokenAmount,
    /// Set aside for swaps in flight.
    #[n(1)]
    pub reserved: TokenAmount,
}

/// Why a pocket cannot make a move.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum PocketError {
    #[error("pocket is short: {available} available, {requested} requested")]
    InsufficientAvailable {
        available: TokenAmount,
        requested: TokenAmount,
    },
    #[error("pocket reservation is short: {reserved} reserved, {requested} requested")]
    InsufficientReserved {
        reserved: TokenAmount,
        requested: TokenAmount,
    },
    #[error("pocket balance would overflow")]
    Overflow,
}

impl Pocket {
    /// Liquidity arrives.
    pub fn fund(self, amount: TokenAmount) -> Result<Self, PocketError> {
        Ok(Self {
            available: add(self.available, amount)?,
            ..self
        })
    }

    /// Liquidity leaves `available`, as the source of a rebalance.
    pub fn withdraw(self, amount: TokenAmount) -> Result<Self, PocketError> {
        Ok(Self {
            available: self.take_available(amount)?,
            ..self
        })
    }

    /// `amount` moves from available to reserved.
    pub fn reserve(self, amount: TokenAmount) -> Result<Self, PocketError> {
        Ok(Self {
            available: self.take_available(amount)?,
            reserved: add(self.reserved, amount)?,
        })
    }

    /// `amount` moves from reserved back to available.
    pub fn release(self, amount: TokenAmount) -> Result<Self, PocketError> {
        Ok(Self {
            reserved: self.take_reserved(amount)?,
            available: add(self.available, amount)?,
        })
    }

    /// `amount` leaves reserved for good: it was paid out on-chain.
    pub fn spend(self, amount: TokenAmount) -> Result<Self, PocketError> {
        Ok(Self {
            reserved: self.take_reserved(amount)?,
            ..self
        })
    }

    fn take_available(self, requested: TokenAmount) -> Result<TokenAmount, PocketError> {
        self.available
            .checked_sub(requested)
            .ok_or(PocketError::InsufficientAvailable {
                available: self.available,
                requested,
            })
    }

    fn take_reserved(self, requested: TokenAmount) -> Result<TokenAmount, PocketError> {
        self.reserved
            .checked_sub(requested)
            .ok_or(PocketError::InsufficientReserved {
                reserved: self.reserved,
                requested,
            })
    }
}

fn add(balance: TokenAmount, amount: TokenAmount) -> Result<TokenAmount, PocketError> {
    balance.checked_add(amount).ok_or(PocketError::Overflow)
}

crate::storable_as_cbor!(Swap);
crate::storable_as_cbor!(Pocket);
