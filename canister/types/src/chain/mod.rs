#[cfg(test)]
mod tests;

use ic_stable_structures::storable::{Bound, Storable};
use minicbor::{Decode, Encode};
use std::borrow::Cow;
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

    /// The seconds a block takes, at the most, on the chains the canister knows (rounded up);
    /// twelve, Ethereum's, on any other. What waits for a block past a time reads: too long
    /// only delays a read, never makes it wrong.
    pub const fn block_seconds(self) -> u64 {
        match self.0 {
            // Optimism, Polygon, Base, Avalanche, Linea
            10 | 137 | 8453 | 43114 | 59144 => 2,
            // BSC
            56 => 3,
            // Arbitrum's quarter seconds
            42161 => 1,
            _ => 12,
        }
    }

    /// The chain's name, for the chains the canister signs on or the deploy script adds;
    /// None for any other, which a message names by its id.
    pub const fn name(self) -> Option<&'static str> {
        match self.0 {
            1 => Some("Ethereum"),
            10 => Some("OP Mainnet"),
            130 => Some("Unichain"),
            137 => Some("Polygon"),
            143 => Some("Monad"),
            480 => Some("World Chain"),
            999 => Some("HyperEVM"),
            8453 => Some("Base"),
            9745 => Some("Plasma"),
            42161 => Some("Arbitrum"),
            _ => None,
        }
    }

    /// The symbol of the chain's native coin, which every EVM chain counts in wei (18
    /// decimals); None for a chain [`Self::name`] does not name.
    pub const fn native_symbol(self) -> Option<&'static str> {
        match self.0 {
            1 | 10 | 130 | 480 | 8453 | 42161 => Some("ETH"),
            137 => Some("POL"),
            143 => Some("MON"),
            999 => Some("HYPE"),
            9745 => Some("XPL"),
            _ => None,
        }
    }
}

impl fmt::Display for ChainId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Eight big-endian bytes, so a stable map orders chains by id.
impl Storable for ChainId {
    fn to_bytes(&self) -> Cow<'_, [u8]> {
        Cow::Owned(self.0.to_be_bytes().to_vec())
    }

    fn from_bytes(bytes: Cow<[u8]>) -> Self {
        Self(u64::from_be_bytes(bytes.as_ref().try_into().expect(
            "BUG: a stored chain id is written as exactly 8 bytes",
        )))
    }

    const BOUND: Bound = Bound::Bounded {
        max_size: 8,
        is_fixed_size: true,
    };
}
