use super::*;
use crate::events::TxPurpose;
use ic_stable_structures::Storable;
use std::borrow::Cow;

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
        amount_paid: None,
        waiting_since: None,
        last_leg: None,
        last_outcome: None,
        last_tx_hash: None,
        paid_out: None,
        fee_accrued: None,
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

/// Paid is a fact, not an amount: a payment of zero is still the one payment.
#[test]
fn a_swap_paid_zero_is_paid() {
    assert_eq!(swap(SwapStatus::Executing).ensure_unpaid(), Ok(()));
    let paid_zero = Swap {
        amount_paid: Some(TokenAmount::ZERO),
        ..swap(SwapStatus::Executing)
    };
    assert_eq!(paid_zero.ensure_unpaid(), Err(TransitionError::AlreadyPaid));
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

/// The waiting index walks its keys oldest first: a stored key orders by when the wait began,
/// then by swap id, in its bytes as well as in `Ord`.
#[test]
fn waiting_keys_order_by_wait_then_swap_and_round_trip() {
    use ic_stable_structures::Storable;

    let key = |nanos: u64, byte: u8| WaitingKey {
        since: Timestamp::from_nanos(nanos),
        quote_hash: QuoteHash::new([byte; 32]),
    };
    let keys = [
        key(1, 9),
        key(255, 0),
        key(256, 0),
        key(256, 1),
        key(u64::MAX, 0),
    ];
    assert!(keys.windows(2).all(|pair| pair[0] < pair[1]));
    let bytes: Vec<_> = keys.iter().map(|k| k.to_bytes().into_owned()).collect();
    assert!(bytes.windows(2).all(|pair| pair[0] < pair[1]));
    for k in keys {
        assert_eq!(k.to_bytes().len(), 40);
        assert_eq!(WaitingKey::from_bytes(k.to_bytes()), k);
    }
}

/// Swaps and pockets live in stable maps, so both must survive their encoding exactly.
#[test]
fn swaps_and_pockets_round_trip_through_storage() {
    use ic_stable_structures::Storable;

    let swap = Swap {
        quote_bytes: vec![0xde, 0xad],
        last_attempt: Some(Attempt::new(3)),
        open_attempt: Some(Attempt::new(3)),
        amount_paid: Some(TokenAmount::from(u128::MAX)),
        waiting_since: Some(Timestamp::from_nanos(7)),
        ..swap(SwapStatus::WaitingForUser)
    };
    assert_eq!(Swap::from_bytes(swap.to_bytes()), swap);
    let pocket = Pocket {
        available: TokenAmount::MAX,
        reserved: amount(1),
    };
    assert_eq!(Pocket::from_bytes(pocket.to_bytes()), pocket);
}

/// The deep audit saves its fold between steps, index included, so a key must also
/// survive the minicbor encoding that snapshot is written in.
#[test]
fn a_waiting_key_round_trips_through_minicbor() {
    let key = WaitingKey {
        since: Timestamp::from_nanos(1_700_000_000_123_456_789),
        quote_hash: QuoteHash::new([0x5a; 32]),
    };
    let bytes = minicbor::to_vec(key).unwrap();
    assert_eq!(minicbor::decode::<WaitingKey>(&bytes).unwrap(), key);
}

/// The wait the auto-refund index holds for a swap is the one the timer can act on: the
/// swap waits for its user, its quote asks for an automatic refund, and the clock runs. A
/// swap that waits for a human, or whose bytes do not read as a quote, or whose clock is
/// not running, is not the timer's work and has none.
#[test]
fn auto_refund_wait_is_the_wait_the_timer_can_act_on() {
    use crate::quote::tests::fixed_quote;
    use crate::quote::Quote;

    let since = Timestamp::from_nanos(7);
    let waiting = |quote: &Quote| Swap {
        quote_bytes: quote.canonical_bytes().unwrap(),
        waiting_since: Some(since),
        ..swap(SwapStatus::WaitingForUser)
    };
    let auto = fixed_quote();
    assert!(auto.auto_refund, "the fixture asks for an automatic refund");
    let manual = Quote {
        auto_refund: false,
        ..fixed_quote()
    };
    assert_eq!(waiting(&auto).auto_refund_wait(), Some(since));
    assert_eq!(
        waiting(&manual).auto_refund_wait(),
        None,
        "waits for a human"
    );
    assert_eq!(
        Swap {
            quote_bytes: vec![0xff; 9],
            ..waiting(&auto)
        }
        .auto_refund_wait(),
        None,
        "bytes that are no quote"
    );
    assert_eq!(
        Swap {
            waiting_since: None,
            ..waiting(&auto)
        }
        .auto_refund_wait(),
        None,
        "no clock running"
    );
    assert_eq!(
        Swap {
            status: SwapStatus::Executing,
            ..waiting(&auto)
        }
        .auto_refund_wait(),
        None,
        "not waiting"
    );
}

/// The engine drives a swap by which leg its latest attempt was and how it ended, and the
/// fold learns both from the lines it already has: a leg from the purpose of the number
/// the signed record spends, and an outcome from the line that closes the attempt. A
/// swap that has signed nothing knows neither, and both are absent from its stored bytes,
/// so a swap stored before the fields existed reads back the same.
#[test]
fn a_leg_is_read_off_the_purpose_and_neither_is_stored_while_unknown() {
    let qh = QuoteHash::new([1; 32]);
    assert_eq!(Leg::of(TxPurpose::Burn(qh)), Some(Leg::Burn));
    assert_eq!(Leg::of(TxPurpose::Mint(qh)), Some(Leg::Mint));
    assert_eq!(Leg::of(TxPurpose::Payout(qh)), Some(Leg::Payout));
    assert_eq!(Leg::of(TxPurpose::Refund(qh)), Some(Leg::Refund));
    assert_eq!(Leg::of(TxPurpose::Reclaim(qh)), Some(Leg::Reclaim));
    assert_eq!(
        Leg::of(TxPurpose::GaslessPull(qh)),
        None,
        "a pull is no swap's leg"
    );
    assert_eq!(Leg::of(TxPurpose::Cancel(ChainId::BASE)), None);

    // the two ways `TxPurpose` is partitioned agree: every purpose that is a swap's leg
    // names the swap, and the only purpose that names no swap at all is the cancel
    for purpose in [
        TxPurpose::Burn(qh),
        TxPurpose::Mint(qh),
        TxPurpose::Payout(qh),
        TxPurpose::Refund(qh),
        TxPurpose::Reclaim(qh),
        TxPurpose::GaslessPull(qh),
        TxPurpose::Cancel(ChainId::BASE),
    ] {
        if Leg::of(purpose).is_some() {
            assert_eq!(
                purpose.quote_hash(),
                Some(qh),
                "{purpose:?} is a leg of a swap it does not name"
            );
        }
        assert_eq!(
            purpose.quote_hash().is_none(),
            matches!(purpose, TxPurpose::Cancel(_)),
            "{purpose:?} is the only kind of purpose that names no swap"
        );
    }

    let fresh = swap(SwapStatus::FundsReceived);
    assert_eq!((fresh.last_leg, fresh.last_outcome), (None, None));
    let bytes = fresh.to_bytes().into_owned();
    assert_eq!(
        bytes[0], 0x87,
        "seven fields: every trailing field that is not known is absent, the two new ones \
         included"
    );
    let failed = Swap {
        last_leg: Some(Leg::Mint),
        last_outcome: Some(Outcome::Failed),
        ..swap(SwapStatus::Executing)
    };
    let bytes = failed.to_bytes().into_owned();
    assert_eq!(bytes[0], 0x8b, "eleven fields once both are known");
    assert_eq!(Swap::from_bytes(Cow::Owned(bytes)), failed);
}
