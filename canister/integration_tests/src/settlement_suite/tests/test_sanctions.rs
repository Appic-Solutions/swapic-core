//! The sanctions set from outside: who may write it, what a write answers, and what
//! survives an upgrade.

use crate::client::settlement::set_sanctioned;
use crate::settlement_suite::init::{quoter, setup, upgrade, watcher};
use candid::Principal;
use settlement_api::types::errors::{GuardError, SetSanctionedError};

const SANCTIONED: &str = "0x1111111111111111111111111111111111111111";
const ANOTHER: &str = "0x2222222222222222222222222222222222222222";

fn stranger() -> Principal {
    Principal::from_slice(&[9; 29])
}

/// The watcher pushes the list and a controller corrects it; the count answered is what
/// the set holds when the call is done, and a spelling of an address is the address.
#[test]
fn the_watcher_and_a_controller_write_the_set_and_read_back_its_size() {
    let (pic, canister, admin) = setup();
    assert_eq!(
        set_sanctioned(&pic, canister, watcher(), &[SANCTIONED, ANOTHER], &[]),
        Ok(2)
    );
    assert_eq!(
        set_sanctioned(
            &pic,
            canister,
            admin,
            &[],
            &[&SANCTIONED.to_ascii_uppercase().replace("0X", "0x")]
        ),
        Ok(1),
        "a controller removes by the bytes, whatever the spelling"
    );
    assert_eq!(
        set_sanctioned(&pic, canister, watcher(), &[ANOTHER], &[]),
        Ok(1),
        "what is held is not held twice"
    );

    upgrade(&pic, canister, admin).expect("the upgrade goes through");
    assert_eq!(
        set_sanctioned(&pic, canister, admin, &[], &[]),
        Ok(1),
        "the set is stable memory, so it comes through the upgrade"
    );
}

/// Neither a stranger nor the quoter writes compliance data, and a refusal names no
/// principal and changes nothing.
#[test]
fn a_stranger_and_the_quoter_are_refused_and_write_nothing() {
    let (pic, canister, admin) = setup();
    for who in [stranger(), quoter(), Principal::anonymous()] {
        assert_eq!(
            set_sanctioned(&pic, canister, who, &[SANCTIONED], &[]),
            Err(SetSanctionedError::Guard(
                GuardError::CallerNotWatcherOrController
            ))
        );
    }
    assert_eq!(
        set_sanctioned(&pic, canister, admin, &[], &[]),
        Ok(0),
        "nothing landed"
    );
}

/// Text above the address cap is refused by its position in its list, and nothing of the
/// call lands, so the list an operator sends is the list the set holds or none of it.
#[test]
fn a_text_over_the_cap_is_refused_by_position_and_nothing_lands() {
    let (pic, canister, admin) = setup();
    let long = "a".repeat(257);
    assert_eq!(
        set_sanctioned(&pic, canister, watcher(), &[SANCTIONED, &long], &[]),
        Err(SetSanctionedError::TextTooLong {
            list: "add".to_string(),
            index: 1,
            len: 257,
        })
    );
    assert_eq!(
        set_sanctioned(&pic, canister, watcher(), &[], &[&long]),
        Err(SetSanctionedError::TextTooLong {
            list: "remove".to_string(),
            index: 0,
            len: 257,
        })
    );
    assert_eq!(set_sanctioned(&pic, canister, admin, &[], &[]), Ok(0));
}
