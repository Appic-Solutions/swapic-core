//! How an ICP token reaches ckUSDC (the fixed interface section 3c): directly, through its
//! own ckUSDC pool on ICPSwap, or through ICP, two legs (the token and ICP, then ICP and
//! ckUSDC on the ICP/ckUSDC pool). No token has a size cap of its own (Jeff, 2026-10-21): the
//! pool's per swap cap applies, the quote shows the price impact and `min_out` protects the
//! user. From P11b on every path of the pool reads these.
//!
//! Stored as minicbor: `#[n]` indices are append-only, never renumbered or reused, and a new
//! field is optional.

#[cfg(test)]
mod tests;

use super::{principal_cbor, LedgerFee, PrincipalBytes};
use crate::numeric::TokenAmount;
use candid::Principal;
use minicbor::{Decode, Encode};
use thiserror::Error;

/// The pool a route's token trades in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub enum IcpRouteVia {
    /// The token's own ckUSDC pool.
    #[n(0)]
    Direct(#[cbor(n(0), with = "principal_cbor")] Principal),
    /// The token's ICP pool: the route goes through ICP, on the ICP/ckUSDC pool as well.
    #[n(1)]
    Icp(#[cbor(n(0), with = "principal_cbor")] Principal),
}

/// One listed token.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct IcpRoute {
    #[cbor(n(0), with = "principal_cbor")]
    pub ledger: Principal,
    #[n(1)]
    pub via: IcpRouteVia,
}

/// The routes, the ICP/ckUSDC pool, the ICP ledger read off that pool, and the fee of every
/// ledger a route touches, read when they were set.
#[derive(Clone, Debug, Default, PartialEq, Eq, Encode, Decode)]
pub struct IcpRoutes {
    #[n(0)]
    pub icp_ckusdc_pool: Option<PrincipalBytes>,
    /// ICP's ledger: the ICP/ckUSDC pool's token that is not ckUSDC.
    #[n(1)]
    pub icp_ledger: Option<PrincipalBytes>,
    #[n(2)]
    pub routes: Vec<IcpRoute>,
    #[n(3)]
    pub ledger_fees: Vec<LedgerFee>,
}

/// The ICPSwap legs of a path, by the pools they trade in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoutePath {
    /// ckUSDC itself: no leg.
    Ckusdc,
    /// One leg, on the token's ckUSDC pool.
    Direct { pool: Principal },
    /// Two legs: ckUSDC and ICP on `first` (the ICP/ckUSDC pool), ICP and the token on
    /// `second`.
    ViaIcp {
        first: Principal,
        second: Principal,
        icp: Principal,
    },
}

impl RoutePath {
    /// How many ICPSwap legs it has.
    pub fn legs(&self) -> u32 {
        match self {
            RoutePath::Ckusdc => 0,
            RoutePath::Direct { .. } => 1,
            RoutePath::ViaIcp { .. } => 2,
        }
    }
}

/// Why routes are not ones the pool runs on.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RoutesError {
    #[error("{0} is listed twice")]
    Duplicate(Principal),
    #[error("{0} is routed through ICP and no ICP/ckUSDC pool is set")]
    NoIcpPool(Principal),
    #[error("{0} names its own ledger, ckUSDC or the ICP/ckUSDC pool as its own pool")]
    BadPool(Principal),
    #[error("ckUSDC is listed with a pool other than its own ledger")]
    CkusdcPool,
    #[error("ICP is routed through ICP")]
    IcpThroughIcp,
    #[error("no fee was read for {0}")]
    MissingFee(Principal),
    #[error("the ICP ledger is not known")]
    NoIcpLedger,
}

impl IcpRoutes {
    pub fn icp_pool(&self) -> Option<Principal> {
        self.icp_ckusdc_pool.map(|pool| pool.0)
    }

    pub fn icp(&self) -> Option<Principal> {
        self.icp_ledger.map(|ledger| ledger.0)
    }

    /// The routes hold together, `ckusdc` being the pool's ckUSDC ledger: no ledger twice, a
    /// route through ICP only with the ICP/ckUSDC pool set, a pool that is
    /// neither the token's ledger nor ckUSDC's, ckUSDC (if listed) with its own ledger as its
    /// pool, the ICP ledger known when the ICP/ckUSDC pool is set, and ICP never through
    /// itself.
    pub fn validate(&self, ckusdc: Principal) -> Result<(), RoutesError> {
        let mut seen = std::collections::BTreeSet::new();
        if self.icp_ckusdc_pool.is_some() && self.icp_ledger.is_none() {
            return Err(RoutesError::NoIcpLedger);
        }
        for route in &self.routes {
            if !seen.insert(route.ledger) {
                return Err(RoutesError::Duplicate(route.ledger));
            }
            match route.via {
                IcpRouteVia::Direct(pool) if route.ledger == ckusdc => {
                    if pool != ckusdc {
                        return Err(RoutesError::CkusdcPool);
                    }
                }
                IcpRouteVia::Direct(pool) => {
                    if pool == route.ledger || pool == ckusdc {
                        return Err(RoutesError::BadPool(route.ledger));
                    }
                }
                IcpRouteVia::Icp(pool) => {
                    let icp_pool = self
                        .icp_pool()
                        .ok_or(RoutesError::NoIcpPool(route.ledger))?;
                    if Some(route.ledger) == self.icp() || route.ledger == ckusdc {
                        return Err(RoutesError::IcpThroughIcp);
                    }
                    if pool == route.ledger || pool == ckusdc || pool == icp_pool {
                        return Err(RoutesError::BadPool(route.ledger));
                    }
                }
            }
        }
        Ok(())
    }

    /// Every ledger whose fee a route's legs read: each listed token's, and ICP's.
    pub fn ledgers(&self) -> Vec<Principal> {
        let mut out: Vec<Principal> = self.routes.iter().map(|route| route.ledger).collect();
        if let Some(icp) = self.icp() {
            if !out.contains(&icp) {
                out.push(icp);
            }
        }
        out
    }

    /// A fee was read for every ledger a route touches (`ckusdc` is the settings').
    pub fn fees_complete(&self, ckusdc: Principal) -> Result<(), RoutesError> {
        for ledger in self.ledgers() {
            if ledger != ckusdc && self.fee_of(ledger).is_none() {
                return Err(RoutesError::MissingFee(ledger));
            }
        }
        Ok(())
    }

    pub fn fee_of(&self, ledger: Principal) -> Option<TokenAmount> {
        self.ledger_fees
            .iter()
            .find(|fee| fee.ledger == ledger)
            .map(|fee| fee.fee)
    }

    pub fn route_of(&self, ledger: Principal) -> Option<&IcpRoute> {
        self.routes.iter().find(|route| route.ledger == ledger)
    }

    /// The path a token takes to or from ckUSDC, None for a token not listed.
    pub fn path_of(&self, ledger: Principal, ckusdc: Principal) -> Option<RoutePath> {
        if ledger == ckusdc {
            return Some(RoutePath::Ckusdc);
        }
        Some(match self.route_of(ledger)?.via {
            IcpRouteVia::Direct(pool) => RoutePath::Direct { pool },
            IcpRouteVia::Icp(pool) => RoutePath::ViaIcp {
                first: self.icp_pool()?,
                second: pool,
                icp: self.icp()?,
            },
        })
    }
}
