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

use crate::hash::TxHash;
use crate::numeric::{BlockNumber, TokenAmount};
use minicbor::{Decode, Encode};
use thiserror::Error;

/// One chain's book lines and its last balance check. `in_flight` is not here: it is the
/// USDC the flows hold on the chain, read off the flows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Encode, Decode)]
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
        }
    }
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
            BookStep::Checked { .. } => {}
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

    /// Records a step [`Books::admits`] admitted.
    pub fn apply(mut self, step: &BookStep) -> Self {
        match step {
            BookStep::FeeEarned { amount, .. } => {
                self.fee_income = self
                    .fee_income
                    .checked_add(*amount)
                    .unwrap_or(TokenAmount::MAX);
            }
            BookStep::Checked { check, .. } => self.last_check = Some(*check),
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

crate::storable_as_cbor!(Books);
