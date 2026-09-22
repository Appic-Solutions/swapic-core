//! What the canister knows about a chain's head and its gas prices, and how long that
//! knowledge stays good for. The watcher pushes readings; the canister stamps them, because
//! the age of a reading is what every money decision on it depends on.

#[cfg(test)]
mod tests;

use crate::numeric::{BlockNumber, GasAmount, Timestamp, Wei, WeiPerGas};
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

    /// What to send at on this reading: the tip it suggests, and a ceiling of twice the
    /// base fee on top of it, which carries a transaction through several blocks of a
    /// rising base fee. `None` when the arithmetic leaves 256 bits or either field is above
    /// [`MAX_FEE_PER_GAS`].
    pub fn fees(&self) -> Option<Fees> {
        let max_fee = self
            .base_fee
            .checked_mul(2_u8)?
            .checked_add(self.priority_fee)?;
        Fees::new(max_fee, self.priority_fee)
    }

    /// The most a replacement priced off this reading may offer: [`MAX_FEE_MULTIPLE`] times
    /// the base fee plus the tip, and never above [`MAX_FEE_PER_GAS`].
    ///
    /// Arithmetic that leaves 256 bits saturates at the absolute ceiling rather than
    /// answering nothing, because a reading that large is one no replacement should be
    /// priced off and the bound is exactly what says so.
    pub fn fee_ceiling(&self) -> Fees {
        let ceiling = self
            .base_fee
            .checked_add(self.priority_fee)
            .and_then(|going_rate| going_rate.checked_mul(MAX_FEE_MULTIPLE))
            .unwrap_or(MAX_FEE_PER_GAS)
            .min(MAX_FEE_PER_GAS);
        Fees {
            max_fee: ceiling,
            max_priority_fee: ceiling,
        }
    }
}

/// The absolute ceiling on either fee field of any transaction this canister signs: a
/// thousand gwei per gas.
///
/// Two orders of magnitude above the worst congestion any chain this canister sends to has
/// seen, and far below what a compromised or broken watcher could push, so it is a backstop
/// and never a constraint on a real send. It is a constant and not a config knob on
/// purpose: the knob belongs with the engine's other gas policy, in a later plan, and until
/// then the bound cannot be raised by a config write.
pub const MAX_FEE_PER_GAS: WeiPerGas = WeiPerGas::new(1_000_000_000_000);

/// How far above the freshest reading a replacement may bid: eight times the base fee plus
/// the tip it suggests.
///
/// A replacement a node accepts pays at least an eighth more than the one it replaces, so
/// the bump doubles; three doublings is already eight times, and past that the chain is not
/// refusing the price, it is refusing the transaction. A config knob for this belongs with
/// the engine, in a later plan.
pub const MAX_FEE_MULTIPLE: u8 = 8;

/// What one transaction offers to pay for its gas: a ceiling per unit, and the tip inside
/// it that the block producer actually keeps.
///
/// A pair rather than two loose values, because the two are the same type and swapping them
/// compiles: a transaction whose tip is its ceiling and whose ceiling is its tip would offer
/// the producer everything. Every one of them is built here, and every one is inside
/// [`MAX_FEE_PER_GAS`] with its tip no higher than its ceiling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fees {
    max_fee: WeiPerGas,
    max_priority_fee: WeiPerGas,
}

impl Fees {
    /// The pair `max_fee` and `max_priority_fee` name, with the tip held to the ceiling it
    /// is paid out of, or nothing at all when either is above [`MAX_FEE_PER_GAS`].
    ///
    /// Under EIP-1559 the producer keeps min(tip, ceiling - base fee), so a tip above the
    /// ceiling is not an error the chain reports, it is a number that quietly means
    /// something else. Clamping is the honest reading of it, and the absolute bound is what
    /// stops a reading this canister did not compute from pricing a transaction.
    pub fn new(max_fee: WeiPerGas, max_priority_fee: WeiPerGas) -> Option<Self> {
        if max_fee > MAX_FEE_PER_GAS || max_priority_fee > MAX_FEE_PER_GAS {
            return None;
        }
        // a tip above its own ceiling is the ceiling
        Some(
            Self {
                max_fee,
                max_priority_fee,
            }
            .clamped(),
        )
    }

    fn clamped(self) -> Self {
        Self {
            max_fee: self.max_fee,
            max_priority_fee: self.max_priority_fee.min(self.max_fee),
        }
    }

    pub fn max_fee(&self) -> WeiPerGas {
        self.max_fee
    }

    pub fn max_priority_fee(&self) -> WeiPerGas {
        self.max_priority_fee
    }

    /// What `gas` units of gas cost at this ceiling, which is the most the transaction can
    /// spend. `None` above 256 bits.
    pub fn worst_cost(&self, gas: GasAmount) -> Option<Wei> {
        self.max_fee.transaction_cost(gas)
    }

    /// This pair with both fields held to `cap`: what a ceiling becomes when a bound other
    /// than the per-gas one, such as the most a whole transaction may cost, sits below
    /// it. A cap above both fields changes nothing.
    pub fn capped(self, cap: WeiPerGas) -> Self {
        Self {
            max_fee: self.max_fee.min(cap),
            max_priority_fee: self.max_priority_fee.min(cap),
        }
        .clamped()
    }

    /// The fees a replacement pays: double what the transaction being replaced offered, but
    /// never below what the chain is asking now and never above `ceiling`.
    ///
    /// `None` once neither field can be raised any further, which is not a failure: a node
    /// accepts a replacement only when both fields are higher than the ones it replaces, so
    /// a bid at the ceiling is the last bid there is. The transaction is not abandoned, it
    /// keeps going out at the price it already offers, because a chain that will not mine
    /// eight times the going rate will not mine sixteen either.
    pub fn bumped(self, floor: Fees, ceiling: Fees) -> Option<Self> {
        let bump = |old: WeiPerGas, floor: WeiPerGas, cap: WeiPerGas| {
            old.checked_mul(2_u8).unwrap_or(cap).max(floor).min(cap)
        };
        let bumped = Self {
            max_fee: bump(self.max_fee, floor.max_fee, ceiling.max_fee),
            max_priority_fee: bump(
                self.max_priority_fee,
                floor.max_priority_fee,
                ceiling.max_priority_fee,
            ),
        }
        .clamped();
        let raised =
            bumped.max_fee > self.max_fee && bumped.max_priority_fee > self.max_priority_fee;
        raised.then_some(bumped)
    }
}

/// Why a pushed reading is not one this canister prices gas with.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ChainDataError {
    #[error("{field} does not fit in 256 bits")]
    FeeTooLarge { field: &'static str },
}

crate::storable_as_cbor!(ChainData);
