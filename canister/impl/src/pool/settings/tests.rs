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
