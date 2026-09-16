use super::*;

#[test]
fn only_done_refunded_and_frozen_are_closed() {
    use SwapStatus::*;
    for status in [Done, Refunded, Frozen] {
        assert!(status.is_closed(), "{status:?}");
    }
    for status in [
        FundsReceived,
        Executing,
        PaidInStable,
        Delivering,
        WaitingForUser,
        Refunding,
    ] {
        assert!(!status.is_closed(), "{status:?}");
    }
}

#[test]
fn a_new_pocket_is_empty() {
    let pocket = Pocket::default();
    assert_eq!(pocket.available, TokenAmount::ZERO);
    assert_eq!(pocket.reserved, TokenAmount::ZERO);
}
