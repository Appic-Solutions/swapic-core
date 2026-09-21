//! What the canister knows about a chain's head and its gas prices, and how long that
//! knowledge stays good for. The watcher pushes readings; the canister stamps them, because
//! the age of a reading is what every money decision on it depends on.

#[cfg(test)]
mod tests;

use crate::numeric::{BlockNumber, Timestamp, WeiPerGas};
use minicbor::{Decode, Encode};
use std::time::Duration;
use thiserror::Error;

/// One chain as the watcher last saw it: the head it had and what gas cost there. It
/// carries no instant, so only the canister can say when it arrived.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChainReading {
    pub block: BlockNumber,
    /// The base fee of the head block.
    pub base_fee: WeiPerGas,
    /// The tip the watcher suggests paying on top of the base fee.
    pub priority_fee: WeiPerGas,
}

impl ChainReading {
    /// The reading as the cache holds it, dated by the canister's own clock.
    pub fn pushed_at(self, at: Timestamp) -> ChainData {
        let Self {
            block,
            base_fee,
            priority_fee,
        } = self;
        ChainData {
            block,
            base_fee,
            priority_fee,
            pushed_at: at,
        }
    }
}

/// A reading with the instant the canister received it.
///
/// Stored as minicbor: `#[n]` indices are append-only, never renumbered or reused, and a
/// new field is optional.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct ChainData {
    #[n(0)]
    pub block: BlockNumber,
    #[n(1)]
    pub base_fee: WeiPerGas,
    #[n(2)]
    pub priority_fee: WeiPerGas,
    /// Canister time, never the caller's.
    #[n(3)]
    pub pushed_at: Timestamp,
}

impl ChainData {
    /// Whether the reading is young enough to decide on at `now`. The cap is inclusive, and
    /// a stamp ahead of the clock has no age rather than a negative one.
    pub fn is_fresh(&self, now: Timestamp, max_age: Duration) -> bool {
        now.saturating_duration_since(self.pushed_at) <= max_age
    }

    /// The reading without its stamp.
    pub fn reading(&self) -> ChainReading {
        ChainReading {
            block: self.block,
            base_fee: self.base_fee,
            priority_fee: self.priority_fee,
        }
    }
}

/// Why a pushed reading is not one this canister prices gas with.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ChainDataError {
    #[error("{field} does not fit in 256 bits")]
    FeeTooLarge { field: &'static str },
}

crate::storable_as_cbor!(ChainData);
