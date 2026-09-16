#[cfg(test)]
mod tests;

use candid::Nat;
use minicbor::data::{IanaTag, Type};
use minicbor::decode::{Decoder, Error as DecodeError};
use minicbor::encode::{Encoder, Error as EncodeError, Write};
use num_bigint::BigUint;
use std::cmp::Ordering;
use std::fmt;
use std::marker::PhantomData;
use std::num::ParseIntError;
use std::ops::Rem;
use thiserror::Error;

/// `CheckedAmountOf<Unit>` keeps an amount of some `Unit`. Every operation is checked and
/// returns `None` instead of overflowing.
///
/// # Arithmetic
/// ```
/// use types::checked_amount::CheckedAmountOf;
///
/// enum MetricApple {}
/// type Apples = CheckedAmountOf<MetricApple>;
///
/// let three_apples = Apples::from(3_u8);
///
/// // Checked addition
/// assert_eq!(three_apples.checked_add(Apples::TWO), Some(Apples::from(5_u8)));
/// assert_eq!(Apples::MAX.checked_add(Apples::ONE), None);
///
/// // Checked subtraction
/// assert_eq!(three_apples.checked_sub(Apples::TWO), Some(Apples::ONE));
/// assert_eq!(Apples::TWO.checked_sub(three_apples), None);
///
/// // Checked multiplication by scalar
/// assert_eq!(three_apples.checked_mul(2_u8), Some(Apples::from(6_u8)));
/// assert_eq!(Apples::MAX.checked_mul(2_u8), None);
///
/// // Ceiling checked division by scalar
/// assert_eq!(three_apples.checked_div_ceil(0_u8), None);
/// assert_eq!(three_apples.checked_div_ceil(2_u8), Some(Apples::TWO));
///
/// // Flooring checked division by scalar (Euclidean division)
/// assert_eq!(three_apples.checked_div_floor(0_u8), None);
/// assert_eq!(three_apples.checked_div_floor(2_u8), Some(Apples::ONE));
/// assert_eq!(three_apples.checked_div_ceil(3_u8), Some(Apples::ONE));
/// ```
pub struct CheckedAmountOf<Unit>(ethnum::u256, PhantomData<Unit>);

impl<Unit> CheckedAmountOf<Unit> {
    pub const ZERO: Self = Self(ethnum::u256::ZERO, PhantomData);
    pub const ONE: Self = Self(ethnum::u256::ONE, PhantomData);
    pub const TWO: Self = Self(ethnum::u256::new(2), PhantomData);
    pub const MAX: Self = Self(ethnum::u256::MAX, PhantomData);

    /// `new` is a synonym for `from` that can be evaluated at compile time, for constants.
    #[inline]
    pub const fn new(value: u128) -> Self {
        Self(ethnum::u256::new(value), PhantomData)
    }

    #[inline]
    const fn from_inner(value: ethnum::u256) -> Self {
        Self(value, PhantomData)
    }

    pub const fn into_inner(self) -> ethnum::u256 {
        self.0
    }

    pub fn from_str_hex(src: &str) -> Result<Self, ParseIntError> {
        ethnum::u256::from_str_hex(src).map(Self::from_inner)
    }

    pub fn from_be_bytes(bytes: [u8; 32]) -> Self {
        Self::from_inner(ethnum::u256::from_be_bytes(bytes))
    }

    pub fn to_be_bytes(self) -> [u8; 32] {
        self.0.to_be_bytes()
    }

    /// The amount as a `u128`, or `None` above `u128::MAX`. The canonical codecs write
    /// amounts in 16 bytes, so they read them through this.
    pub fn try_into_u128(self) -> Option<u128> {
        u128::try_from(self.0).ok()
    }

    pub fn checked_add(self, other: Self) -> Option<Self> {
        self.0.checked_add(other.0).map(Self::from_inner)
    }

    pub fn checked_sub(self, other: Self) -> Option<Self> {
        self.0.checked_sub(other.0).map(Self::from_inner)
    }

    pub fn change_units<NewUnits>(self) -> CheckedAmountOf<NewUnits> {
        CheckedAmountOf::<NewUnits>::from_inner(self.0)
    }

    pub fn checked_mul<T: Into<ethnum::u256>>(self, factor: T) -> Option<Self> {
        self.0.checked_mul(factor.into()).map(Self::from_inner)
    }

    pub fn checked_div_ceil<T: Into<ethnum::u256>>(self, rhs: T) -> Option<Self> {
        let rhs = rhs.into();
        if rhs == ethnum::u256::ZERO {
            return None;
        }
        let (quotient, remainder) = (self.0.div_euclid(rhs), self.0.rem(&rhs));
        if remainder == ethnum::u256::ZERO {
            Some(Self::from_inner(quotient))
        } else {
            Self::from_inner(quotient).checked_add(Self::ONE)
        }
    }

    pub fn checked_div_floor<T: Into<ethnum::u256>>(self, rhs: T) -> Option<Self> {
        let rhs = rhs.into();
        if rhs == ethnum::u256::ZERO {
            return None;
        }
        Some(Self::from_inner(self.0.div_euclid(rhs)))
    }
}

macro_rules! impl_from {
    ($($t:ty),* $(,)?) => {$(
        impl<Unit> From<$t> for CheckedAmountOf<Unit> {
            #[inline]
            fn from(value: $t) -> Self {
                Self(ethnum::u256::from(value), PhantomData)
            }
        }
    )*};
}

impl_from! { u8, u16, u32, u64, u128 }

/// A `Nat` too large for 256 bits.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("{0} does not fit in 256 bits")]
pub struct NatOverflow(pub Nat);

impl<Unit> TryFrom<Nat> for CheckedAmountOf<Unit> {
    type Error = NatOverflow;

    fn try_from(value: Nat) -> Result<Self, Self::Error> {
        let value_bytes = value.0.to_bytes_be();
        if value_bytes.len() > 32 {
            return Err(NatOverflow(value));
        }
        let mut value_u256 = [0u8; 32];
        value_u256[32 - value_bytes.len()..].copy_from_slice(&value_bytes);
        Ok(Self::from_be_bytes(value_u256))
    }
}

impl<Unit> From<CheckedAmountOf<Unit>> for Nat {
    fn from(value: CheckedAmountOf<Unit>) -> Self {
        Nat::from(BigUint::from_bytes_be(&value.0.to_be_bytes()))
    }
}

impl<Unit> fmt::Debug for CheckedAmountOf<Unit> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl<Unit> fmt::Display for CheckedAmountOf<Unit> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl<Unit> Default for CheckedAmountOf<Unit> {
    fn default() -> Self {
        Self::ZERO
    }
}

impl<Unit> Clone for CheckedAmountOf<Unit> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<Unit> Copy for CheckedAmountOf<Unit> {}

impl<Unit> PartialEq for CheckedAmountOf<Unit> {
    fn eq(&self, rhs: &Self) -> bool {
        self.0.eq(&rhs.0)
    }
}

impl<Unit> Eq for CheckedAmountOf<Unit> {}

impl<Unit> PartialOrd for CheckedAmountOf<Unit> {
    fn partial_cmp(&self, rhs: &Self) -> Option<Ordering> {
        Some(self.cmp(rhs))
    }
}

impl<Unit> Ord for CheckedAmountOf<Unit> {
    fn cmp(&self, rhs: &Self) -> Ordering {
        self.0.cmp(&rhs.0)
    }
}

/// Up to `u64::MAX` a native CBOR unsigned integer, above it a positive bignum (tag 2 and
/// the big-endian bytes without leading zeros).
impl<C, Unit> minicbor::Encode<C> for CheckedAmountOf<Unit> {
    fn encode<W: Write>(
        &self,
        e: &mut Encoder<W>,
        _ctx: &mut C,
    ) -> Result<(), EncodeError<W::Error>> {
        match u64::try_from(self.0) {
            Ok(n) => e.u64(n)?.ok(),
            Err(_) => {
                let bytes = self.0.to_be_bytes();
                let start = (self.0.leading_zeros() / 8) as usize;
                e.tag(IanaTag::PosBignum)?.bytes(&bytes[start..])?.ok()
            }
        }
    }
}

impl<'b, C, Unit> minicbor::Decode<'b, C> for CheckedAmountOf<Unit> {
    fn decode(d: &mut Decoder<'b>, _ctx: &mut C) -> Result<Self, DecodeError> {
        match d.datatype()? {
            Type::U8 | Type::U16 | Type::U32 | Type::U64 => d.u64().map(Self::from),
            Type::Tag => {
                if d.tag()? != IanaTag::PosBignum {
                    return Err(DecodeError::message("amount: expected a positive bignum"));
                }
                let bytes = d.bytes()?;
                if bytes.len() > 32 {
                    return Err(DecodeError::message(format!(
                        "amount: {} bytes do not fit in 256 bits",
                        bytes.len()
                    )));
                }
                let mut be_bytes = [0u8; 32];
                be_bytes[32 - bytes.len()..].copy_from_slice(bytes);
                Ok(Self::from_be_bytes(be_bytes))
            }
            other => Err(DecodeError::message(format!(
                "amount: expected an unsigned integer or a bignum, found {other}"
            ))),
        }
    }
}
