use super::*;

fn amount(value: u128) -> TokenAmount {
    TokenAmount::from(value)
}

fn pocket(available: u128, reserved: u128) -> Pocket {
    Pocket {
        available: amount(available),
        reserved: amount(reserved),
    }
}

fn swap(status: SwapStatus) -> Swap {
    Swap {
        quote_bytes: vec![],
        status,
        last_attempt: None,
        open_attempt: None,
        src_chain: ChainId::BASE,
        src_token: "usdc".parse().unwrap(),
        amount_in: amount(100),
        amount_paid: TokenAmount::ZERO,
        waiting_since: None,
    }
}

#[test]
fn only_done_refunded_and_frozen_are_closed() {
    use SwapStatus::*;
    for status in [Done, Refunded, Frozen] {
        assert!(status.is_closed(), "{status:?}");
        assert_eq!(
            swap(status).ensure_not_closed(),
            Err(TransitionError::SwapClosed(status))
        );
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
        assert_eq!(swap(status).ensure_not_closed(), Ok(()));
    }
}

#[test]
fn attempts_run_one_two_three_without_gaps() {
    let mut s = swap(SwapStatus::Executing);
    assert_eq!(s.next_attempt(), Some(Attempt::FIRST));
    assert_eq!(
        s.ensure_next_attempt(Attempt::new(2)),
        Err(TransitionError::AttemptOutOfSequence {
            attempt: Attempt::new(2),
            expected: Some(Attempt::FIRST)
        })
    );
    s.last_attempt = Some(Attempt::new(2));
    assert_eq!(s.ensure_next_attempt(Attempt::new(3)), Ok(()));
    // the last number there is has no successor
    s.last_attempt = Some(Attempt::new(u32::MAX));
    assert_eq!(s.next_attempt(), None);
    assert!(s.ensure_next_attempt(Attempt::new(u32::MAX)).is_err());
}

#[test]
fn a_new_pocket_is_empty() {
    assert_eq!(Pocket::default(), pocket(0, 0));
}

#[test]
fn reserve_and_release_move_between_available_and_reserved() {
    let p = pocket(1000, 0).reserve(amount(400)).unwrap();
    assert_eq!(p, pocket(600, 400));
    assert_eq!(p.release(amount(150)), Ok(pocket(750, 250)));
    assert_eq!(
        p.reserve(amount(601)),
        Err(PocketError::InsufficientAvailable {
            available: amount(600),
            requested: amount(601)
        })
    );
    assert_eq!(
        p.release(amount(401)),
        Err(PocketError::InsufficientReserved {
            reserved: amount(400),
            requested: amount(401)
        })
    );
}

#[test]
fn spend_and_withdraw_take_value_out_of_the_pocket() {
    assert_eq!(pocket(600, 400).spend(amount(250)), Ok(pocket(600, 150)));
    assert_eq!(pocket(1000, 5).withdraw(amount(250)), Ok(pocket(750, 5)));
    assert!(pocket(0, 5).withdraw(amount(1)).is_err());
    assert!(pocket(5, 0).spend(amount(1)).is_err());
}

#[test]
fn a_pocket_never_overflows_silently() {
    let full = Pocket {
        available: TokenAmount::MAX,
        reserved: TokenAmount::MAX,
    };
    assert_eq!(full.fund(amount(1)), Err(PocketError::Overflow));
    assert_eq!(full.release(amount(1)), Err(PocketError::Overflow));
    assert_eq!(full.reserve(amount(1)), Err(PocketError::Overflow));
}
