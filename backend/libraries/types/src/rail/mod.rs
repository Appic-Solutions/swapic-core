#[cfg(test)]
mod tests;

use crate::chain::ChainId;
use crate::config::ChainTable;
use crate::evm::EvmAddress;
use crate::numeric::UnixSeconds;
use crate::quote::{Quote, QuoteAddressError, QuoteAddressField};
use minicbor::{Decode, Encode};
use std::fmt;
use std::str::FromStr;
use thiserror::Error;

/// The bridge a swap settles over.
///
/// Stored as minicbor: `#[n]` indices are append-only, never renumbered or reused.
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

/// CCTP's id for a chain, which is what a burn names its destination by: Circle's own
/// numbering, read off each chain's `MessageTransmitterV2.localDomain()` and never a chain
/// id.
///
/// Stored as minicbor: `#[n]` indices are append-only, never renumbered or reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Encode, Decode)]
#[cbor(transparent)]
pub struct CctpDomain(#[n(0)] u32);

impl CctpDomain {
    pub const fn new(domain: u32) -> Self {
        Self(domain)
    }

    pub const fn get(self) -> u32 {
        self.0
    }
}

impl fmt::Display for CctpDomain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
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

/// Why a quote is not one its rail can carry: the rails carry the USDC the deploy
/// configured and nothing else, so a quote naming any other token on either side would
/// have the burn or the publish spend the vault's USDC against a deposit of something
/// else, or pay out a token the mint never delivered.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RailTokenError {
    #[error("the quote's {field} is {quoted}, and the {rail} rail carries {rail_token}")]
    NotTheRailToken {
        field: QuoteAddressField,
        quoted: EvmAddress,
        rail_token: EvmAddress,
        rail: Rail,
    },
    #[error(transparent)]
    QuoteAddress(#[from] QuoteAddressError),
    #[error("chain {chain_id} has no token configured for the {rail} rail")]
    NoRailToken { chain_id: ChainId, rail: Rail },
}

/// Pins both of a quote's tokens to its rail: the source token must be the source chain's
/// entry in `rail_tokens` and the destination token the destination chain's, compared as
/// addresses and never as text, so no spelling passes as another token. Every rail this
/// canister drives carries the configured USDC on both sides, so one table serves them
/// all. Refused by the field and the reason, before any outcall or any line.
pub fn ensure_rail_tokens(
    rail_tokens: &ChainTable<EvmAddress>,
    quote: &Quote,
) -> Result<(), RailTokenError> {
    let rail = quote.rail;
    for (field, chain_id) in [
        (QuoteAddressField::SrcToken, quote.src_chain),
        (QuoteAddressField::DstToken, quote.dst_chain),
    ] {
        let quoted = quote.evm_address(field)?;
        let rail_token = rail_tokens
            .get(chain_id)
            .ok_or(RailTokenError::NoRailToken { chain_id, rail })?;
        if quoted != rail_token {
            return Err(RailTokenError::NotTheRailToken {
                field,
                quoted,
                rail_token,
                rail,
            });
        }
    }
    Ok(())
}

/// The most bytes an Eco route may be. A quoted route carries the calls the filler runs
/// on the destination, about a kilobyte for a burn; eight leaves room for a longer path.
pub const MAX_ROUTE_BYTES: usize = 8_192;

/// Why an Eco intent was not taken in.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum EcoIntentError {
    #[error("the route is {len} bytes, above the cap of {cap}")]
    RouteTooLong { len: usize, cap: usize },
}

/// What Eco's quote response gives a swap on the Eco rail, handed in by the watcher: the
/// intent's destination as ECO named it, its encoded route, the reward's deadline, and the
/// prover. The destination is Eco's `destinationChainID` and not the chain the user is
/// paid on: for a two-hop route Eco fills on the source chain itself and carries the
/// funds on from there, and an intent published for the final chain is one nobody fills.
///
/// Stored as minicbor: `#[n]` indices are append-only, never renumbered or reused, and a
/// new field is optional.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub struct EcoIntent {
    #[n(0)]
    pub destination: ChainId,
    #[cbor(n(1), with = "minicbor::bytes")]
    pub route: Vec<u8>,
    /// The reward's deadline: the last second a filler may claim it, after which the
    /// refund is permissionless.
    #[n(2)]
    pub deadline: UnixSeconds,
    #[n(3)]
    pub prover: EvmAddress,
}

impl EcoIntent {
    /// An intent inside the bound, or which bound it broke.
    pub fn new(
        destination: ChainId,
        route: Vec<u8>,
        deadline: UnixSeconds,
        prover: EvmAddress,
    ) -> Result<Self, EcoIntentError> {
        if route.len() > MAX_ROUTE_BYTES {
            return Err(EcoIntentError::RouteTooLong {
                len: route.len(),
                cap: MAX_ROUTE_BYTES,
            });
        }
        Ok(Self {
            destination,
            route,
            deadline,
            prover,
        })
    }

    /// Whether the reward's deadline has passed at `now`: the deadline second itself is
    /// still the filler's.
    pub fn is_past_deadline(&self, now: UnixSeconds) -> bool {
        now > self.deadline
    }
}

crate::storable_as_cbor!(EcoIntent);
