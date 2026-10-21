use super::*;
use candid::{Decode, Encode};
use types::{Timestamp, TokenAmount};

#[test]
fn settings_cross_the_wire_and_back() {
    let settings = domain::RebalanceSettings::default();
    let wire = RebalanceSettings::from(&settings);
    assert_eq!(wire.min_move, Nat::from(300_000_000_u64));
    assert_eq!(wire.proposal_window_s, 60);
    let bytes = Encode!(&wire).unwrap();
    let back = Decode!(&bytes, RebalanceSettings).unwrap();
    assert_eq!(domain::RebalanceSettings::try_from(back).unwrap(), settings);
}

/// The view the watcher reads (P13c): the ref is the move's, the window is named, and a
/// stuck move reads `Stuck` whatever its position.
#[test]
fn a_move_reads_as_the_interface_names_it() {
    let mut item = domain::Move::opened(
        7,
        domain::MoveDirection::IcpToBase,
        TokenAmount::from(500_000_000_u64),
        false,
        Timestamp::from_nanos(11),
        None,
        TokenAmount::from(90_000_u64),
    );
    item.status = domain::MoveStatus::Landed;
    item.window_ends = Some(Timestamp::from_nanos(60_000_000_011));
    let view = MoveView::from(&item);
    assert_eq!(view.status, MoveStatus::Landed);
    assert_eq!(view.r#ref, domain::move_ref(7).into_bytes().to_vec());
    assert_eq!(view.window_ends_ns, Some(60_000_000_011));
    assert_eq!(view.own_route_cost, Nat::from(90_000_u64));
    let bytes = Encode!(&view).unwrap();
    assert_eq!(Decode!(&bytes, MoveView).unwrap(), view);
    item.stuck = Some("no arrival".to_string());
    assert_eq!(
        MoveStatus::from(&item),
        MoveStatus::Stuck("no arrival".to_string())
    );
    // the minter's call reads as the move still open, the minter's burn as burned
    item.stuck = None;
    item.status = domain::MoveStatus::Withdrawing;
    assert_eq!(MoveStatus::from(&item), MoveStatus::Open);
    item.status = domain::MoveStatus::Withdrawn;
    assert_eq!(MoveStatus::from(&item), MoveStatus::Burned);
}
