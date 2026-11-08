//! The only two Jupiter instructions Swapic ever calls, and the fields Swapic reads of them.
//!
//! The layouts are proved on 200 captured `/build` answers (see `tests.rs` and
//! `vectors/jupiter_routes.json`): `route_v2` holds its amounts from byte 8, and
//! `shared_accounts_route_v2` from byte 9, after its one-byte authority id.

use crate::errors::SwapicError;

pub const ROUTE_V2: [u8; 8] = [0xbb, 0x64, 0xfa, 0xcc, 0x31, 0xc4, 0xaf, 0x14];
pub const SHARED_ACCOUNTS_ROUTE_V2: [u8; 8] = [0xd1, 0x98, 0x53, 0x93, 0x7c, 0xfe, 0xd8, 0xe9];

/// True when the data names one of the two routes the program lets through (attack H1).
pub fn is_allowed_route(data: &[u8]) -> bool {
    data.len() >= 8 && (data[..8] == ROUTE_V2 || data[..8] == SHARED_ACCOUNTS_ROUTE_V2)
}

/// `route_v2`'s fixed accounts: where the authority and the user's token accounts sit.
pub const ROUTE_V2_AUTHORITY: usize = 0;
pub const ROUTE_V2_SOURCE: usize = 1;
pub const ROUTE_V2_DESTINATION: usize = 2;
pub const ROUTE_V2_SOURCE_MINT: usize = 3;
pub const ROUTE_V2_DESTINATION_MINT: usize = 4;
pub const ROUTE_V2_DESTINATION_OPTIONAL: usize = 7;
/// `shared_accounts_route_v2`'s.
pub const SHARED_AUTHORITY: usize = 1;
pub const SHARED_SOURCE: usize = 2;
pub const SHARED_DESTINATION: usize = 5;
pub const SHARED_SOURCE_MINT: usize = 6;
pub const SHARED_DESTINATION_MINT: usize = 7;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RouteKind {
    RouteV2,
    SharedAccountsRouteV2,
}

/// The fields of a route's arguments Swapic reads. The steps after them are Jupiter's alone.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Route {
    pub kind: RouteKind,
    pub in_amount: u64,
    pub quoted_out: u64,
    pub slippage_bps: u16,
    pub platform_fee_bps: u16,
    pub positive_slippage_bps: u16,
}

impl Route {
    pub fn parse(data: &[u8]) -> Result<Self, SwapicError> {
        if data.len() < 8 {
            return Err(SwapicError::JupiterInstructionNotAllowed);
        }
        let (kind, at) = if data[..8] == ROUTE_V2 {
            (RouteKind::RouteV2, 8)
        } else if data[..8] == SHARED_ACCOUNTS_ROUTE_V2 {
            (RouteKind::SharedAccountsRouteV2, 9)
        } else {
            return Err(SwapicError::JupiterInstructionNotAllowed);
        };
        // the amounts (8 + 8), the slippage and the two fee fields (2 + 2 + 2), the steps' length (4)
        if data.len() < at + 26 {
            return Err(SwapicError::JupiterInstructionNotAllowed);
        }
        let u64_at = |i: usize| {
            let mut b = [0u8; 8];
            b.copy_from_slice(&data[i..i + 8]);
            u64::from_le_bytes(b)
        };
        let u16_at = |i: usize| u16::from_le_bytes([data[i], data[i + 1]]);
        Ok(Self {
            kind,
            in_amount: u64_at(at),
            quoted_out: u64_at(at + 8),
            slippage_bps: u16_at(at + 16),
            platform_fee_bps: u16_at(at + 18),
            positive_slippage_bps: u16_at(at + 20),
        })
    }

    /// The route's own minimum out: `quoted_out` less `slippage_bps`, as Jupiter's program applies it.
    pub fn minimum_out(&self) -> u64 {
        let keep = 10_000u128.saturating_sub(self.slippage_bps as u128);
        ((self.quoted_out as u128 * keep) / 10_000) as u64
    }

    pub const fn source_index(&self) -> usize {
        match self.kind {
            RouteKind::RouteV2 => ROUTE_V2_SOURCE,
            RouteKind::SharedAccountsRouteV2 => SHARED_SOURCE,
        }
    }

    pub const fn authority_index(&self) -> usize {
        match self.kind {
            RouteKind::RouteV2 => ROUTE_V2_AUTHORITY,
            RouteKind::SharedAccountsRouteV2 => SHARED_AUTHORITY,
        }
    }

    pub const fn destination_index(&self) -> usize {
        match self.kind {
            RouteKind::RouteV2 => ROUTE_V2_DESTINATION,
            RouteKind::SharedAccountsRouteV2 => SHARED_DESTINATION,
        }
    }
}

#[cfg(test)]
mod tests;
