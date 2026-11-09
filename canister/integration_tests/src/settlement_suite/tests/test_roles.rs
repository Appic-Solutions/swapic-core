//! The roles cell and its one door, `set_roles`: the install names both roles, a controller
//! rotates them, and a watcher's door (`relay_gasless`) shows who holds the watcher's role.
//! No door asks for the quoter since the v1 quote doors went; the cell still holds it.

use crate::client::settlement::{events_page, set_roles};
use crate::settlement_suite::init::{quoter, setup_with_rail_usdc, upgrade, watcher};
use candid::{decode_one, encode_args, Principal};
use pocket_ic::PocketIc;
use settlement_api::types::chain_data::ChainData;
use settlement_api::types::errors::{Role, SetRolesError};
use settlement_api::types::events::{Event, EventType};

fn stranger() -> Principal {
    Principal::from_slice(&[9; 29])
}

fn events(pic: &PocketIc, canister: Principal) -> Vec<Event> {
    events_page(pic, canister, stranger(), 0, 100)
}

/// Whether `caller` holds the watcher's role: `relay_gasless` checks the role before it
/// reads anything, so a stranger is refused for the role and the watcher for its bytes.
fn is_watcher(pic: &PocketIc, canister: Principal, caller: Principal, block: u64) -> bool {
    let reading = ChainData {
        block,
        base_fee_wei_per_gas: 1_000_000_000_u64.into(),
        priority_fee_wei_per_gas: 100_000_000_u64.into(),
    };
    let args = encode_args((8453_u64, vec![0_u8], vec![0_u8], 300_000_u64, Some(reading)))
        .expect("relay args are candid");
    let answer = pic
        .update_call(canister, caller, "relay_gasless", args)
        .expect("the relay door answers");
    !matches!(
        decode_one::<Result<u64, String>>(&answer).expect("a relay answer"),
        Err(why) if why.contains("CallerNotRole")
    )
}

#[test]
fn set_roles_refuses_a_stranger_and_changes_nothing() {
    let (pic, canister, _) = setup_with_rail_usdc();
    assert!(set_roles(&pic, canister, stranger(), quoter(), stranger()).is_err());
    assert!(!is_watcher(&pic, canister, stranger(), 1));
    assert!(is_watcher(&pic, canister, watcher(), 2));
}

/// Rotation is the point of holding the roles in a writable cell: a second `set_roles`
/// must revoke the old watcher, not add a second one.
#[test]
fn set_roles_rotates_the_watcher_and_the_old_one_loses_access() {
    let (pic, canister, admin) = setup_with_rail_usdc();
    let next = Principal::from_slice(&[4; 29]);
    set_roles(&pic, canister, admin, quoter(), next).unwrap();

    assert!(is_watcher(&pic, canister, next, 1));
    assert!(
        !is_watcher(&pic, canister, watcher(), 2),
        "the replaced watcher is out"
    );
}

/// The anonymous principal is every unauthenticated caller at once, so handing it a role
/// would open the endpoint to the world.
#[test]
fn set_roles_refuses_the_anonymous_principal() {
    let (pic, canister, admin) = setup_with_rail_usdc();
    let err = set_roles(&pic, canister, admin, Principal::anonymous(), watcher())
        .expect_err("anonymous is not a service identity");
    assert_eq!(err, SetRolesError::AnonymousRole(Role::Quoter), "say why");
    let err = set_roles(&pic, canister, admin, quoter(), Principal::anonymous())
        .expect_err("either slot");
    assert_eq!(err, SetRolesError::AnonymousRole(Role::Watcher), "say why");
}

/// A rotation changes who may move money, so it leaves an audit line naming both
/// principals in the world-readable log.
#[test]
fn set_roles_lands_a_roles_changed_event() {
    let (pic, canister, admin) = setup_with_rail_usdc();
    let before = events(&pic, canister).len();
    let next = Principal::from_slice(&[4; 29]);
    set_roles(&pic, canister, admin, next, watcher()).unwrap();
    let logged = events(&pic, canister);
    assert_eq!(logged.len(), before + 1, "one rotation, one event");
    assert_eq!(
        logged.last().unwrap().payload,
        EventType::RolesChanged {
            quoter: next.to_text(),
            watcher: watcher().to_text(),
        }
    );
}

/// A refused rotation must leave no audit line either: the event and the cell move
/// together or not at all.
#[test]
fn a_refused_set_roles_writes_no_event() {
    let (pic, canister, admin) = setup_with_rail_usdc();
    let before = events(&pic, canister);
    assert!(set_roles(&pic, canister, stranger(), quoter(), watcher()).is_err());
    assert!(set_roles(&pic, canister, admin, Principal::anonymous(), watcher()).is_err());
    assert_eq!(events(&pic, canister), before);
}

/// Everything the canister holds is in stable memory: the roles come through an upgrade
/// untouched, with no second `set_roles`.
#[test]
fn the_roles_survive_an_upgrade() {
    let (pic, canister, admin) = setup_with_rail_usdc();
    let next = Principal::from_slice(&[4; 29]);
    set_roles(&pic, canister, admin, quoter(), next).unwrap();

    upgrade(&pic, canister, admin).unwrap();

    assert!(is_watcher(&pic, canister, next, 1));
    assert!(!is_watcher(&pic, canister, watcher(), 2));
    assert!(!is_watcher(&pic, canister, stranger(), 3));
}
