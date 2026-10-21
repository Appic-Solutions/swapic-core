use super::*;
use candid::{Nat, Principal};

fn wire() -> WireSettings {
    WireSettings {
        tvl_cap: Nat::from(10_000_000_000_u64),
        per_swap_cap: Nat::from(1_000_000_000_u64),
        premium_bps: 10,
        ckusdc_ledger: Principal::from_text(types::pool::CKUSDC_LEDGER).unwrap(),
        icp_tokens: vec![],
        deposit_grace_s: 120,
        max_open_per_payer: 3,
        max_held_bps: 5_000,
        arrival_backup_s: 300,
        pool_open_for_swaps: false,
    }
}

#[test]
fn set_pool_settings_refuses_a_stranger_a_halt_and_bad_settings() {
    assert!(checked(false, true, wire()).is_ok());
    assert_eq!(
        checked(false, false, wire()),
        Err(PoolError::NotAllowed(
            "only a controller sets the pool's settings".to_string()
        ))
    );
    assert_eq!(checked(true, true, wire()), Err(PoolError::Halted));
    let swap_above_tvl = WireSettings {
        per_swap_cap: Nat::from(20_000_000_000_u64),
        ..wire()
    };
    assert!(matches!(
        checked(false, true, swap_above_tvl),
        Err(PoolError::BadSettings(_))
    ));
    let too_wide = WireSettings {
        tvl_cap: Nat::from(u128::MAX) + Nat::from(1_u8),
        ..wire()
    };
    assert!(matches!(
        checked(false, true, too_wide),
        Err(PoolError::BadSettings(_))
    ));
    let held_above_all = WireSettings {
        max_held_bps: 10_001,
        ..wire()
    };
    assert!(matches!(
        checked(false, true, held_above_all),
        Err(PoolError::BadSettings(_))
    ));
}

fn payout_wire() -> settlement_api::types::pool::PayoutSettings {
    settlement_api::types::pool::PayoutSettings {
        min_network_fee: candid::Nat::from(50_000_u64),
        rebalance_share_floor_bps: 2,
        order_wait_s: 120,
        max_payout_gas: 2_000_000,
    }
}

#[test]
fn set_payout_settings_refuses_a_stranger_a_halt_and_bad_settings() {
    assert_eq!(
        payout_checked(true, true, payout_wire()),
        Err(PoolError::Halted)
    );
    assert!(matches!(
        payout_checked(false, false, payout_wire()),
        Err(PoolError::NotAllowed(_))
    ));
    let bad = settlement_api::types::pool::PayoutSettings {
        max_payout_gas: 1,
        ..payout_wire()
    };
    assert!(matches!(
        payout_checked(false, true, bad),
        Err(PoolError::BadSettings(_))
    ));
    let bad = settlement_api::types::pool::PayoutSettings {
        order_wait_s: 0,
        ..payout_wire()
    };
    assert!(matches!(
        payout_checked(false, true, bad),
        Err(PoolError::BadSettings(_))
    ));
    assert_eq!(
        payout_checked(false, true, payout_wire()),
        Ok(types::pool::PayoutSettings::default())
    );
}

#[test]
fn set_icp_routes_refuses_a_stranger_and_a_halt_and_reads_icp_off_its_pool() {
    let ckusdc = candid::Principal::from_text(types::pool::CKUSDC_LEDGER).unwrap();
    let icp = candid::Principal::from_text("ryjl3-tyaaa-aaaaa-aaaba-cai").unwrap();
    assert_eq!(
        routes_checked(true, true, None, Vec::new()),
        Err(PoolError::Halted)
    );
    assert!(matches!(
        routes_checked(false, false, None, Vec::new()),
        Err(PoolError::NotAllowed(_))
    ));
    let route = settlement_api::types::pool::IcpRoute {
        ledger: candid::Principal::from_slice(&[9; 10]),
        via: settlement_api::types::pool::IcpRouteVia::Icp(candid::Principal::from_slice(&[8; 10])),
    };
    let routes = routes_checked(false, true, Some(icp), vec![route]).unwrap();
    assert_eq!(routes.icp_pool(), Some(icp));
    assert_eq!(routes.routes.len(), 1);
    assert_eq!(
        icp_ledger_of(&ckusdc.to_text(), &icp.to_text(), ckusdc),
        Some(icp)
    );
    assert_eq!(
        icp_ledger_of(&icp.to_text(), &ckusdc.to_text(), ckusdc),
        Some(icp)
    );
    assert_eq!(icp_ledger_of(&icp.to_text(), &icp.to_text(), ckusdc), None);
    assert_eq!(
        icp_ledger_of("not a principal", &ckusdc.to_text(), ckusdc),
        None
    );
}
