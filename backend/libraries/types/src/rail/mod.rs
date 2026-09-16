#[cfg(test)]
mod tests;

use minicbor::{Decode, Encode};
use std::fmt;
use std::str::FromStr;
use thiserror::Error;

/// The bridge a swap settles over.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Encode, Decode)]
#[cbor(index_only)]
pub enum Rail {
    #[n(0)]
    CctpV2Fast,
    #[n(1)]
    CctpV2Standard,
    #[n(2)]
    Eco,
}

impl Rail {
    /// The rail's id, as quotes carry it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CctpV2Fast => "cctp_v2_fast",
            Self::CctpV2Standard => "cctp_v2_standard",
            Self::Eco => "eco",
        }
    }
}

/// Text that names no rail.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("{0:?} is not a rail")]
pub struct UnknownRail(pub String);

impl FromStr for Rail {
    type Err = UnknownRail;

    fn from_str(id: &str) -> Result<Self, Self::Err> {
        match id {
            "cctp_v2_fast" => Ok(Self::CctpV2Fast),
            "cctp_v2_standard" => Ok(Self::CctpV2Standard),
            "eco" => Ok(Self::Eco),
            other => Err(UnknownRail(other.to_string())),
        }
    }
}

impl fmt::Display for Rail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
