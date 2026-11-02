//! The halt switch: controller-only, and it outlives an upgrade.

use crate::client::settlement::{self, set_halted};
use crate::settlement_suite::init::{quoter, setup_with_rail_usdc, upgrade};
use candid::Principal;
use pocket_ic::PocketIc;

fn stranger() -> Principal {
    Principal::from_slice(&[9; 29])
}

fn halted(pic: &PocketIc, canister: Principal) -> bool {
    settlement::halted(pic, canister, stranger())
}

#[test]
fn set_halted_refuses_a_stranger() {
    let (pic, canister, admin) = setup_with_rail_usdc();
    assert!(set_halted(&pic, canister, stranger(), true).is_err());
    assert!(set_halted(&pic, canister, quoter(), true).is_err());
    assert!(!halted(&pic, canister));
    set_halted(&pic, canister, admin, true).unwrap();
    assert!(halted(&pic, canister));
}

/// An upgrade is the first thing ops reach for, so the flag is stable: a halt
/// outlives the redeploy and only a deliberate call clears it.
#[test]
fn the_halt_flag_survives_an_upgrade() {
    let (pic, canister, admin) = setup_with_rail_usdc();
    set_halted(&pic, canister, admin, true).unwrap();

    upgrade(&pic, canister, admin).unwrap();

    assert!(halted(&pic, canister), "a redeploy is not an investigation");
    set_halted(&pic, canister, admin, false).unwrap();
    assert!(!halted(&pic, canister));
}
