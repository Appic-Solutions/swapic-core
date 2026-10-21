use super::*;
use candid::Nat;

fn wire() -> WireSettings {
    WireSettings::from(&RebalanceSettings::default())
}

#[test]
fn settings_are_set_by_a_controller_and_hold_together() {
    assert_eq!(checked(true, true, wire()), Err(PoolError::Halted));
    assert!(matches!(
        checked(false, false, wire()),
        Err(PoolError::NotAllowed(_))
    ));
    assert_eq!(checked(false, true, wire()), Ok(RebalanceSettings::default()));
    let bad = WireSettings {
        max_move: Nat::from(1_u8),
        ..wire()
    };
    assert!(matches!(
        checked(false, true, bad),
        Err(PoolError::BadSettings(_))
    ));
}
