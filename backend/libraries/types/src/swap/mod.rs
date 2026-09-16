#[cfg(test)]
mod tests;

use crate::address::TokenId;
use crate::chain::ChainId;
use crate::hash::QuoteHash;
use crate::numeric::{Attempt, Timestamp, TokenAmount};
use thiserror::Error;

/// Where a swap is in its lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwapStatus {
    FundsReceived,
    Executing,
    PaidInStable,
    Delivering,
    WaitingForUser,
    Done,
    Refunding,
    Refunded,
    Frozen,
}

impl SwapStatus {
    /// Terminal: no further work is ever scheduled for the swap.
    pub fn is_closed(self) -> bool {
        matches!(self, Self::Done | Self::Refunded | Self::Frozen)
    }
}

/// The folded state of one swap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Swap {
    /// The canonical preimage of the quote the funds arrived for.
    pub quote_bytes: Vec<u8>,
    pub status: SwapStatus,
    /// The latest attempt signed, open or not.
    pub last_attempt: Option<Attempt>,
    /// The attempt signed and not yet confirmed or failed.
    pub open_attempt: Option<Attempt>,
    pub src_chain: ChainId,
    pub src_token: TokenId,
    pub amount_in: TokenAmount,
    /// Zero until the swap is paid in stable.
    pub amount_paid: TokenAmount,
    /// When the swap paused on the user, while it waits.
    pub waiting_since: Option<Timestamp>,
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
    #[error("swap is not executing ({0:?})")]
    NotExecuting(SwapStatus),
    #[error("swap is already paid in stable")]
    AlreadyPaid,
    #[error("swap cannot start a refund ({0:?})")]
    CannotStartRefund(SwapStatus),
    #[error("swap is not refunding ({0:?})")]
    NotRefunding(SwapStatus),
    #[error("swap is not in flight ({0:?})")]
    NotInFlight(SwapStatus),
    #[error("accrued fees would overflow")]
    FeesOverflow,
    #[error(transparent)]
    Pocket(#[from] PocketError),
}

impl Swap {
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

    /// A swap is paid in stable at most once, so a requote cannot overwrite the amount.
    pub fn ensure_unpaid(&self) -> Result<(), TransitionError> {
        if self.amount_paid != TokenAmount::ZERO {
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

/// One chain's liquidity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pocket {
    /// Free to reserve or rebalance.
    pub available: TokenAmount,
    /// Set aside for swaps in flight.
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
