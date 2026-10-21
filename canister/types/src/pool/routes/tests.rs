use super::*;
use crate::pool::LedgerFee;

fn principal(byte: u8) -> Principal {
    Principal::from_slice(&[byte; 10])
}

fn ckusdc() -> Principal {
    principal(0x01)
}

fn icp() -> Principal {
    principal(0x02)
}

fn icp_pool() -> Principal {
    principal(0x03)
}

fn route(ledger: u8, via: IcpRouteVia) -> IcpRoute {
    IcpRoute {
        ledger: principal(ledger),
        via,
    }
}

fn routes() -> IcpRoutes {
    IcpRoutes {
        icp_ckusdc_pool: Some(PrincipalBytes(icp_pool())),
        icp_ledger: Some(PrincipalBytes(icp())),
        routes: vec![
            route(0x02, IcpRouteVia::Direct(icp_pool())),
            route(0x10, IcpRouteVia::Direct(principal(0x11))),
            route(0x20, IcpRouteVia::Icp(principal(0x21))),
            route(0x01, IcpRouteVia::Direct(ckusdc())),
        ],
        ledger_fees: [0x02, 0x10, 0x20]
            .into_iter()
            .map(|byte| LedgerFee {
                ledger: principal(byte),
                fee: TokenAmount::from(u32::from(byte)),
            })
            .collect(),
    }
}

#[test]
fn a_token_is_routed_directly_or_through_icp() {
    let routes = routes();
    assert_eq!(routes.validate(ckusdc()), Ok(()));
    assert_eq!(routes.fees_complete(ckusdc()), Ok(()));
    assert_eq!(routes.path_of(ckusdc(), ckusdc()), Some(RoutePath::Ckusdc));
    assert_eq!(
        routes.path_of(principal(0x10), ckusdc()),
        Some(RoutePath::Direct {
            pool: principal(0x11)
        })
    );
    assert_eq!(
        routes.path_of(principal(0x20), ckusdc()),
        Some(RoutePath::ViaIcp {
            first: icp_pool(),
            second: principal(0x21),
            icp: icp(),
        })
    );
    assert_eq!(routes.path_of(principal(0x30), ckusdc()), None);
    assert_eq!(RoutePath::Ckusdc.legs(), 0);
    assert_eq!(routes.path_of(principal(0x20), ckusdc()).unwrap().legs(), 2);
}

#[test]
fn routes_that_do_not_hold_together_are_refused() {
    let with = |change: &dyn Fn(&mut IcpRoutes)| {
        let mut routes = routes();
        change(&mut routes);
        routes.validate(ckusdc())
    };
    assert_eq!(
        with(&|routes| routes
            .routes
            .push(route(0x10, IcpRouteVia::Direct(principal(0x12))))),
        Err(RoutesError::Duplicate(principal(0x10)))
    );
    assert_eq!(
        with(&|routes| routes.icp_ckusdc_pool = None),
        Err(RoutesError::NoIcpPool(principal(0x20)))
    );
    assert_eq!(
        with(&|routes| routes.icp_ledger = None),
        Err(RoutesError::NoIcpLedger)
    );
    assert_eq!(
        with(&|routes| routes.routes[1].via = IcpRouteVia::Direct(ckusdc())),
        Err(RoutesError::BadPool(principal(0x10)))
    );
    assert_eq!(
        with(&|routes| routes.routes[2].via = IcpRouteVia::Icp(icp_pool())),
        Err(RoutesError::BadPool(principal(0x20)))
    );
    assert_eq!(
        with(&|routes| routes.routes[0].via = IcpRouteVia::Icp(principal(0x22))),
        Err(RoutesError::IcpThroughIcp)
    );
    assert_eq!(
        with(&|routes| routes.routes[3].via = IcpRouteVia::Direct(principal(0x23))),
        Err(RoutesError::CkusdcPool)
    );
    let mut missing = routes();
    missing.ledger_fees.pop();
    assert_eq!(
        missing.fees_complete(ckusdc()),
        Err(RoutesError::MissingFee(principal(0x20)))
    );
}
