#[cfg(test)]
mod tests;

use minicbor::{Decode, Encode};
use std::fmt;

/// An EVM chain id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Encode, Decode)]
#[cbor(transparent)]
pub struct ChainId(#[n(0)] u64);

impl ChainId {
    pub const ETHEREUM: Self = Self(1);
    pub const BSC: Self = Self(56);
    pub const POLYGON: Self = Self(137);
    pub const BASE: Self = Self(8453);
    pub const ARBITRUM: Self = Self(42161);

    pub const fn new(id: u64) -> Self {
        Self(id)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for ChainId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
