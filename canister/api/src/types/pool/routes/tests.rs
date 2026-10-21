use super::*;
use candid::{Decode, Encode};

#[test]
fn a_route_crosses_the_wire_and_back() {
    let route = IcpRoute {
        ledger: Principal::from_slice(&[1; 10]),
        via: IcpRouteVia::Icp(Principal::from_slice(&[2; 10])),
    };
    let bytes = Encode!(&route).unwrap();
    assert_eq!(Decode!(&bytes, IcpRoute).unwrap(), route);
    let domain = domain::IcpRoute::from(route.clone());
    assert_eq!(
        domain.via,
        domain::IcpRouteVia::Icp(Principal::from_slice(&[2; 10]))
    );
    assert_eq!(IcpRoute::from(&domain), route);
    let view = IcpRoutesView::from(None);
    assert_eq!(view.icp_ckusdc_pool, None);
    assert!(view.routes.is_empty());
    let bytes = Encode!(&view).unwrap();
    assert_eq!(Decode!(&bytes, IcpRoutesView).unwrap(), view);
}
