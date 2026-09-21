#[cfg(test)]
mod tests;

use crate::checked_amount::CheckedAmountOf;
use candid::Nat;
use minicbor::{Decode, Encode};
use std::fmt;
use std::num::ParseIntError;
use std::str::FromStr;
use std::time::Duration;

pub enum TokenTag {}
/// An amount of any token in its smallest denomination: quote amounts, event amounts,
/// pocket balances and fees.
pub type TokenAmount = CheckedAmountOf<TokenTag>;

impl TokenAmount {
    /// An amount a canonical preimage can hold: `None` above `u128::MAX`.
    pub fn from_canonical_nat(value: Nat) -> Option<Self> {
        Self::try_from(value)
            .ok()
            .filter(|amount| amount.try_into_u128().is_some())
    }
}

pub enum UsdTag {}
/// Whole US dollars.
pub type UsdAmount = CheckedAmountOf<UsdTag>;

pub enum WeiTag {}
/// The native currency of an EVM chain, in its smallest denomination.
pub type Wei = CheckedAmountOf<WeiTag>;

pub enum WeiPerGasTag {}
/// A gas price: wei paid for each unit of gas.
pub type WeiPerGas = CheckedAmountOf<WeiPerGasTag>;

pub enum GasTag {}
/// Units of gas: a limit, or what a transaction used.
pub type GasAmount = CheckedAmountOf<GasTag>;

impl WeiPerGas {
    /// What `gas` units of gas cost at this price. `None` above 256 bits, so a price and a
    /// limit that cannot both be paid are caught before a transaction is built.
    pub fn transaction_cost(self, gas: GasAmount) -> Option<Wei> {
        self.into_inner()
            .checked_mul(gas.into_inner())
            // the product is an amount of wei: the per-gas unit cancels against the gas
            .map(|total| Wei::from_be_bytes(total.to_be_bytes()))
    }
}

/// A block height on an EVM chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Encode, Decode)]
#[cbor(transparent)]
pub struct BlockNumber(#[n(0)] u64);

impl BlockNumber {
    pub const fn new(height: u64) -> Self {
        Self(height)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

/// How many blocks deep a transaction must be before it counts as confirmed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Encode, Decode)]
#[cbor(transparent)]
pub struct BlockDepth(#[n(0)] u64);

impl BlockDepth {
    pub const fn new(blocks: u64) -> Self {
        Self(blocks)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

/// The number of a transaction attempt within one swap. Attempts count from one, without
/// gaps.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Encode, Decode)]
#[cbor(transparent)]
pub struct Attempt(#[n(0)] u32);

impl Attempt {
    pub const FIRST: Self = Self(1);

    pub const fn new(number: u32) -> Self {
        Self(number)
    }

    pub const fn get(self) -> u32 {
        self.0
    }

    /// The attempt after this one, or `None` past `u32::MAX`.
    pub fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

/// The position of an event in the log, counted from zero.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Encode, Decode)]
#[cbor(transparent)]
pub struct EventIndex(#[n(0)] u64);

impl EventIndex {
    pub const ZERO: Self = Self(0);

    pub const fn new(index: u64) -> Self {
        Self(index)
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    /// The index after this one, or `None` past `u64::MAX`.
    pub fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

/// The transaction count of an account on one chain: the number the next transaction from
/// it must carry. Allocated by the canister and never read from a chain to decide (rule
/// A4), so two transactions can never share one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Encode, Decode)]
#[cbor(transparent)]
pub struct Nonce(#[n(0)] u64);

impl Nonce {
    /// The nonce of an account that has never sent anything.
    pub const ZERO: Self = Self(0);

    pub const fn new(nonce: u64) -> Self {
        Self(nonce)
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    /// The nonce after this one, or `None` past `u64::MAX`.
    pub fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

impl FromStr for Nonce {
    type Err = ParseIntError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        text.parse().map(Self)
    }
}

/// A fraction in hundredths of a percent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Encode, Decode)]
#[cbor(transparent)]
pub struct BasisPoints(#[n(0)] u16);

impl BasisPoints {
    /// One hundred percent.
    pub const MAX: Self = Self(10_000);

    pub const fn new(bps: u16) -> Self {
        Self(bps)
    }

    pub const fn get(self) -> u16 {
        self.0
    }

    /// At most one hundred percent.
    pub fn is_valid(self) -> bool {
        self <= Self::MAX
    }

    /// This fraction of `amount`, rounded down. `None` only on overflow.
    pub fn apply_to(self, amount: TokenAmount) -> Option<TokenAmount> {
        amount
            .checked_mul(self.0)
            .and_then(|scaled| scaled.checked_div_floor(Self::MAX.0))
    }
}

const NANOS_PER_SEC: u64 = 1_000_000_000;

/// IC time: nanoseconds since the Unix epoch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Encode, Decode)]
#[cbor(transparent)]
pub struct Timestamp(#[n(0)] u64);

impl Timestamp {
    pub const fn from_nanos(nanos: u64) -> Self {
        Self(nanos)
    }

    pub const fn as_nanos(self) -> u64 {
        self.0
    }

    /// `None` past the last second u64 nanoseconds can hold, in the year 2554.
    pub fn from_secs(secs: u64) -> Option<Self> {
        secs.checked_mul(NANOS_PER_SEC).map(Self)
    }

    /// Whole seconds, rounded down.
    pub const fn as_secs(self) -> UnixSeconds {
        UnixSeconds(self.0 / NANOS_PER_SEC)
    }

    /// `None` past what u64 nanoseconds can hold.
    pub fn checked_add(self, duration: Duration) -> Option<Self> {
        let nanos = u64::try_from(duration.as_nanos()).ok()?;
        self.0.checked_add(nanos).map(Self)
    }

    /// `None` before the epoch.
    pub fn checked_sub(self, duration: Duration) -> Option<Self> {
        let nanos = u64::try_from(duration.as_nanos()).ok()?;
        self.0.checked_sub(nanos).map(Self)
    }

    /// How long ago `earlier` was. An instant that is not earlier has no age rather than a
    /// negative one, so a clock that moved back reports nothing instead of underflowing.
    pub fn saturating_duration_since(self, earlier: Self) -> Duration {
        Duration::from_nanos(self.0.saturating_sub(earlier.0))
    }
}

/// Whole seconds since the Unix epoch: the precision a quote's expiry is signed at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Encode, Decode)]
#[cbor(transparent)]
pub struct UnixSeconds(#[n(0)] u64);

impl UnixSeconds {
    pub const fn new(secs: u64) -> Self {
        Self(secs)
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    /// Adds the whole seconds of `duration`. `None` past `u64::MAX` seconds.
    pub fn checked_add(self, duration: Duration) -> Option<Self> {
        self.0.checked_add(duration.as_secs()).map(Self)
    }
}

macro_rules! display_inner {
    ($($t:ty),* $(,)?) => {$(
        impl fmt::Display for $t {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    )*};
}

display_inner! {
    BlockNumber, BlockDepth, Attempt, EventIndex, BasisPoints, Timestamp, UnixSeconds, Nonce
}
