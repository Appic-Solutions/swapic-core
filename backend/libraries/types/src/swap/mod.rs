#[cfg(test)]
mod tests;

use crate::address::TokenId;
use crate::chain::ChainId;
use crate::numeric::{Attempt, Timestamp, TokenAmount};

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

/// One chain's liquidity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pocket {
    /// Free to reserve or rebalance.
    pub available: TokenAmount,
    /// Set aside for swaps in flight.
    pub reserved: TokenAmount,
}
