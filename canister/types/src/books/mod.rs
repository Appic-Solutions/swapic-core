//! The canister's USDC books, one per chain, in micro-USDC (spec 3.3, books rule).
//!
//! The lines say what the canister's USDC on a chain is for. A balance check compares
//! them with the balance read at a confirmed block and records both, and nothing else:
//! a surplus is never booked by the check, only by a controller who has looked at it, and
//! only a shortfall beyond the tolerance pauses the pool.
//!
//! Stored as minicbor: `#[n]` indices are append-only, never renumbered or reused, and a
//! new field is optional.

#[cfg(test)]
mod tests;

use crate::chain::ChainId;
use crate::hash::TxHash;
use crate::ledger::LedgerMeta;
use crate::numeric::{BlockNumber, Timestamp, TokenAmount};
use minicbor::data::Type;
use minicbor::{Decode, Decoder, Encode, Encoder};
use std::time::Duration;
use thiserror::Error;

/// One chain's book lines and its last balance check. `in_flight` is not here: it is the
/// USDC the flows hold on the chain, read off the flows.
#[derive(Clone, Debug, Default, PartialEq, Eq, Encode, Decode)]
pub struct Books {
    /// Network and platform fees Swapic withheld to the canister.
    #[n(0)]
    pub fee_income: TokenAmount,
    /// Surplus a controller booked.
    #[n(1)]
    pub unattributed: TokenAmount,
    /// The pool's pocket on this chain.
    #[n(2)]
    pub pool: TokenAmount,
    #[n(3)]
    pub last_check: Option<BalanceCheck>,
    /// When the last balance check that read a balance was recorded. A failed read never
    /// moves it: the pool's guard measures staleness from here.
    #[n(4)]
    pub checked_at: Option<Timestamp>,
    /// When the last balance read that failed was recorded.
    #[n(5)]
    pub failed_at: Option<Timestamp>,
    /// The fees booked from a block no balance check has read at yet. A fee is booked off
    /// a receipt at the swap depth and the check reads the crediting depth behind the head,
    /// so for a while a booked fee is in the lines and not in the balance the check reads.
    #[n(6)]
    pub unseen_fees: UnseenFees,
}

/// A fee and the block of the receipt it was read off.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct BookedFee {
    #[n(0)]
    pub block: BlockNumber,
    #[n(1)]
    pub amount: TokenAmount,
}

/// The fees booked from blocks no balance check has read at yet, in the order they were
/// booked. Absent from storage while empty, so books written before it existed read back
/// with it empty.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UnseenFees(pub Vec<BookedFee>);

/// Stored as the list, and as nothing at all when it is empty.
impl<C> Encode<C> for UnseenFees {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        self.0.encode(e, ctx)
    }

    fn is_nil(&self) -> bool {
        self.0.is_empty()
    }
}

impl<'b, C> Decode<'b, C> for UnseenFees {
    fn decode(d: &mut Decoder<'b>, ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        // a null placeholder is how a record written with the list absent in the middle
        // reads back, and it means the same as the list missing off the end
        if matches!(d.datatype()?, Type::Null | Type::Undefined) {
            d.skip()?;
            return Ok(Self::default());
        }
        Vec::decode(d, ctx).map(Self)
    }

    fn nil() -> Option<Self> {
        Some(Self::default())
    }
}

/// A balance read at a confirmed block, and what the lines expected there.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct BalanceCheck {
    #[n(0)]
    pub block: BlockNumber,
    #[n(1)]
    pub observed: TokenAmount,
    /// The lines less the outflows signed and not yet confirmed.
    #[n(2)]
    pub expected: TokenAmount,
}

impl BalanceCheck {
    /// `observed - expected`, which a pending outflow or a donation makes positive.
    pub fn surplus(&self) -> i128 {
        let signed = |amount: TokenAmount| {
            amount
                .try_into_u128()
                .and_then(|value| i128::try_from(value).ok())
                .unwrap_or(i128::MAX)
        };
        signed(self.observed).saturating_sub(signed(self.expected))
    }
}

/// One step of a chain's books, as the log records it.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub enum BookStep {
    /// Swapic withheld `amount` to the canister in `tx_hash`, a transaction the canister
    /// signed, read off its receipt at depth.
    #[n(0)]
    FeeEarned {
        #[n(0)]
        tx_hash: TxHash,
        #[n(1)]
        amount: TokenAmount,
        /// The block of that receipt. None on lines written before it was recorded, whose
        /// fee every balance check expects.
        #[n(2)]
        block: Option<BlockNumber>,
    },
    /// A balance check; `shortfall` pauses the pool.
    #[n(1)]
    Checked {
        #[n(0)]
        check: BalanceCheck,
        #[n(1)]
        shortfall: bool,
    },
    /// A controller moved `amount` of the last check's surplus into `unattributed`.
    #[n(2)]
    SurplusBooked {
        #[n(0)]
        amount: TokenAmount,
    },
    /// A balance check that could not read the balance, and the kind of failure:
    /// recorded, never skipped, and it refreshes nothing.
    #[n(3)]
    CheckFailed {
        #[n(0)]
        failure: ReadFailure,
    },
    /// A controller paused the pool, or reopened it, which also clears a shortfall it has
    /// looked at.
    #[n(4)]
    PoolPaused {
        #[n(0)]
        paused: bool,
    },
}

impl BookStep {
    pub fn to_bytes(&self) -> Vec<u8> {
        minicbor::to_vec(self).expect("BUG: encoding into a Vec is infallible")
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        minicbor::decode(bytes).ok()
    }

    pub fn name(&self) -> &'static str {
        match self {
            BookStep::FeeEarned { .. } => "FeeEarned",
            BookStep::Checked { .. } => "Checked",
            BookStep::SurplusBooked { .. } => "SurplusBooked",
            BookStep::CheckFailed { .. } => "CheckFailed",
            BookStep::PoolPaused { .. } => "PoolPaused",
        }
    }
}

/// Why a balance check read no balance: a fixed kind, and an HTTP status at most. Never a
/// provider's or a subnet's text, which can carry the provider's url and the api key in
/// it, and this line is in the world-readable log.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub enum ReadFailure {
    /// The chain has no provider configured.
    #[n(0)]
    NoRpcUrl,
    /// The watcher's pushed head is missing or too old.
    #[n(1)]
    NoChainData,
    /// The chain has no confirmation depth configured.
    #[n(2)]
    NoDepth,
    /// The head is below the confirmation depth.
    #[n(3)]
    BelowDepth,
    /// The canister's EVM address could not be derived.
    #[n(4)]
    NoCanisterAddress,
    /// The outcall did not reach the provider.
    #[n(5)]
    Unreachable,
    /// The provider answered with an HTTP error.
    #[n(6)]
    Http {
        #[n(0)]
        status: u16,
    },
    /// The provider answered the call with an error, a revert included.
    #[n(7)]
    Provider,
    /// The answer was not one the canister reads: malformed, too large, or the wrong shape.
    #[n(8)]
    BadAnswer,
}

/// Why a book step was refused.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum BooksError {
    #[error("a book amount must be above zero")]
    Zero,
    #[error("a book line overflowed")]
    Overflow,
    #[error("{amount} is above the {surplus} surplus the last check found")]
    AboveSurplus { amount: TokenAmount, surplus: i128 },
}

impl Books {
    /// Every rule a step must meet.
    pub fn admits(&self, step: &BookStep) -> Result<(), BooksError> {
        match step {
            BookStep::FeeEarned { amount, .. } => {
                if *amount == TokenAmount::ZERO {
                    return Err(BooksError::Zero);
                }
                self.fee_income
                    .checked_add(*amount)
                    .ok_or(BooksError::Overflow)?;
            }
            BookStep::Checked { .. }
            | BookStep::CheckFailed { .. }
            | BookStep::PoolPaused { .. } => {}
            BookStep::SurplusBooked { amount } => {
                if *amount == TokenAmount::ZERO {
                    return Err(BooksError::Zero);
                }
                let surplus = self.last_check.map_or(0, |check| check.surplus());
                let wanted = amount
                    .try_into_u128()
                    .and_then(|value| i128::try_from(value).ok())
                    .unwrap_or(i128::MAX);
                if wanted > surplus {
                    return Err(BooksError::AboveSurplus {
                        amount: *amount,
                        surplus,
                    });
                }
                self.unattributed
                    .checked_add(*amount)
                    .ok_or(BooksError::Overflow)?;
            }
        }
        Ok(())
    }

    /// Records a step [`Books::admits`] admitted, sealed at `at`.
    pub fn apply(mut self, step: &BookStep, at: Timestamp) -> Self {
        match step {
            BookStep::FeeEarned { amount, block, .. } => {
                self.fee_income = self
                    .fee_income
                    .checked_add(*amount)
                    .unwrap_or(TokenAmount::MAX);
                if let Some(block) = block {
                    self.unseen_fees.0.push(BookedFee {
                        block: *block,
                        amount: *amount,
                    });
                }
            }
            BookStep::Checked { check, .. } => {
                self.last_check = Some(*check);
                self.checked_at = Some(at);
                // a fee at or below the block this check read is in its balance
                self.unseen_fees.0.retain(|fee| fee.block > check.block);
            }
            BookStep::CheckFailed { .. } => self.failed_at = Some(at),
            BookStep::PoolPaused { .. } => {}
            BookStep::SurplusBooked { amount } => {
                self.unattributed = self
                    .unattributed
                    .checked_add(*amount)
                    .unwrap_or(TokenAmount::MAX);
                // the booked part is expected now, so the surplus shrinks by it
                if let Some(check) = self.last_check.as_mut() {
                    check.expected = check
                        .expected
                        .checked_add(*amount)
                        .unwrap_or(TokenAmount::MAX);
                }
            }
        }
        self
    }

    /// The lines the books keep, summed, with the flows' `in_flight` added.
    pub fn total(&self, in_flight: TokenAmount) -> TokenAmount {
        [self.unattributed, self.pool, in_flight]
            .into_iter()
            .try_fold(self.fee_income, TokenAmount::checked_add)
            .unwrap_or(TokenAmount::MAX)
    }

    /// The fees booked from blocks above `block`: in the lines, and not yet in a balance
    /// read at `block`.
    pub fn fees_above(&self, block: BlockNumber) -> TokenAmount {
        self.unseen_fees
            .0
            .iter()
            .filter(|fee| fee.block > block)
            .try_fold(TokenAmount::ZERO, |sum, fee| sum.checked_add(fee.amount))
            .unwrap_or(TokenAmount::MAX)
    }
}

/// What a balance means against the lines: what they expect at that block, and whether it
/// is a shortfall. An outflow signed and not yet confirmed may or may not be mined at the
/// block, so the expectation leaves it out and a pending payout is never a shortfall; a
/// donation reads as surplus and is no shortfall either.
pub fn assess(
    lines: TokenAmount,
    pending_outflows: TokenAmount,
    observed: TokenAmount,
    tolerance: TokenAmount,
) -> (TokenAmount, bool) {
    let expected = lines
        .checked_sub(pending_outflows)
        .unwrap_or(TokenAmount::ZERO);
    let shortfall = observed
        .checked_add(tolerance)
        .is_some_and(|ceiling| ceiling < expected);
    (expected, shortfall)
}

/// How many balance-check intervals the pool chain's books may go without a check that
/// read a balance before the pool closes.
pub const STALE_INTERVALS: u32 = 3;

/// Why the pool is closed. Every pool outflow asks [`pool_open`] first and refuses on any
/// of these: the guard fails closed.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum PoolClosed {
    #[error("the canister is halted")]
    Halted,
    #[error("no pool chain is configured")]
    NoPoolChain,
    #[error("a controller paused the pool")]
    Paused,
    #[error("the balance check on chain {0} found a shortfall")]
    Shortfall(ChainId),
    #[error("the balance check of the ICP pocket found a shortfall")]
    IcpShortfall,
    #[error("the books of chain {0} have never been checked")]
    NeverChecked(ChainId),
    #[error(
        "the books of chain {chain} were last checked {since:?} ago, more than \
         {STALE_INTERVALS} intervals of {interval:?}"
    )]
    Stale {
        chain: ChainId,
        since: Duration,
        interval: Duration,
    },
}

/// Whether the pool may move money at `now`: not halted, a pool chain configured, not
/// paused by a controller, no shortfall latched, and the pool chain's books read within
/// [`STALE_INTERVALS`] check intervals. `pool_books` is the pool chain's.
pub fn pool_open(
    halted: bool,
    pool_chain: Option<ChainId>,
    meta: &LedgerMeta,
    pool_books: Option<Books>,
    interval: Duration,
    now: Timestamp,
) -> Result<(), PoolClosed> {
    if halted {
        return Err(PoolClosed::Halted);
    }
    let chain = pool_chain.ok_or(PoolClosed::NoPoolChain)?;
    if meta.pool_paused_at.is_some() {
        return Err(PoolClosed::Paused);
    }
    if let Some(short) = meta.pool_paused_by {
        return Err(PoolClosed::Shortfall(short));
    }
    let checked_at = pool_books
        .and_then(|books| books.checked_at)
        .ok_or(PoolClosed::NeverChecked(chain))?;
    let since = now.saturating_duration_since(checked_at);
    if since > interval.saturating_mul(STALE_INTERVALS) {
        return Err(PoolClosed::Stale {
            chain,
            since,
            interval,
        });
    }
    Ok(())
}

crate::storable_as_cbor!(Books);
