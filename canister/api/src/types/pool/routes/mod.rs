//! The ICP routes' candid types: interface section 3c, field for field.

#[cfg(test)]
mod tests;

use candid::{CandidType, Principal};
use serde::Deserialize;
use types::pool as domain;

/// The token/ckUSDC pool, or the token/ICP pool.
#[derive(CandidType, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum IcpRouteVia {
    Direct(Principal),
    Icp(Principal),
}

#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct IcpRoute {
    pub ledger: Principal,
    pub via: IcpRouteVia,
}

/// `icp_routes`' answer.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct IcpRoutesView {
    pub icp_ckusdc_pool: Option<Principal>,
    pub routes: Vec<IcpRoute>,
}

impl From<IcpRouteVia> for domain::IcpRouteVia {
    fn from(via: IcpRouteVia) -> Self {
        match via {
            IcpRouteVia::Direct(pool) => Self::Direct(pool),
            IcpRouteVia::Icp(pool) => Self::Icp(pool),
        }
    }
}

impl From<domain::IcpRouteVia> for IcpRouteVia {
    fn from(via: domain::IcpRouteVia) -> Self {
        match via {
            domain::IcpRouteVia::Direct(pool) => Self::Direct(pool),
            domain::IcpRouteVia::Icp(pool) => Self::Icp(pool),
        }
    }
}

impl From<IcpRoute> for domain::IcpRoute {
    fn from(route: IcpRoute) -> Self {
        Self {
            ledger: route.ledger,
            via: route.via.into(),
        }
    }
}

impl From<&domain::IcpRoute> for IcpRoute {
    fn from(route: &domain::IcpRoute) -> Self {
        Self {
            ledger: route.ledger,
            via: route.via.into(),
        }
    }
}

impl From<Option<&domain::IcpRoutes>> for IcpRoutesView {
    fn from(routes: Option<&domain::IcpRoutes>) -> Self {
        match routes {
            Some(routes) => Self {
                icp_ckusdc_pool: routes.icp_pool(),
                routes: routes.routes.iter().map(IcpRoute::from).collect(),
            },
            None => Self {
                icp_ckusdc_pool: None,
                routes: Vec::new(),
            },
        }
    }
}
