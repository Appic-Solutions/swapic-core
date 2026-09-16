#[cfg(test)]
mod tests;

use crate::checked_amount::CheckedAmountOf;
use minicbor::{Decode, Encode};
use std::fmt;
use std::time::Duration;

pub enum TokenTag {}
/// An amount of any token in its smallest denomination: quote amounts, event amounts,
/// pocket balances and fees.
pub type TokenAmount = CheckedAmountOf<TokenTag>;

pub enum UsdTag {}
/// Whole US dollars.
pub type UsdAmount = CheckedAmountOf<UsdTag>;

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

display_inner! { BlockNumber, BlockDepth, Attempt, EventIndex, BasisPoints, Timestamp, UnixSeconds }
