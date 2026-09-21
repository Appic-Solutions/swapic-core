use super::*;

#[test]
fn attempts_count_from_one_and_stop_at_u32_max() {
    assert_eq!(Attempt::FIRST.get(), 1);
    assert_eq!(Attempt::FIRST.next(), Some(Attempt::new(2)));
    assert_eq!(Attempt::new(u32::MAX).next(), None);
}

#[test]
fn event_indexes_count_from_zero_and_stop_at_u64_max() {
    assert_eq!(EventIndex::ZERO.next(), Some(EventIndex::new(1)));
    assert_eq!(EventIndex::new(u64::MAX).next(), None);
}

#[test]
fn basis_points_are_valid_up_to_one_hundred_percent() {
    assert!(BasisPoints::new(0).is_valid());
    assert!(BasisPoints::MAX.is_valid());
    assert!(!BasisPoints::new(10_001).is_valid());
}

#[test]
fn basis_points_take_their_fraction_rounded_down() {
    let amount = TokenAmount::from(25_000_000_u32);
    assert_eq!(
        BasisPoints::new(30).apply_to(amount),
        Some(TokenAmount::from(75_000_u32))
    );
    assert_eq!(BasisPoints::MAX.apply_to(amount), Some(amount));
    assert_eq!(
        BasisPoints::new(0).apply_to(amount),
        Some(TokenAmount::ZERO)
    );
    // 3 bps of 1000 is 0.3, which rounds to nothing
    assert_eq!(
        BasisPoints::new(3).apply_to(TokenAmount::from(1_000_u32)),
        Some(TokenAmount::ZERO)
    );
    assert_eq!(BasisPoints::new(2).apply_to(TokenAmount::MAX), None);
}

#[test]
fn timestamps_convert_between_nanos_and_seconds() {
    let t = Timestamp::from_secs(1_800_000_000).unwrap();
    assert_eq!(t.as_nanos(), 1_800_000_000_000_000_000);
    assert_eq!(t.as_secs(), UnixSeconds::new(1_800_000_000));
    // rounded down
    assert_eq!(
        Timestamp::from_nanos(1_999_999_999).as_secs(),
        UnixSeconds::new(1)
    );
    assert_eq!(Timestamp::from_secs(u64::MAX), None);
}

#[test]
fn timestamps_add_durations_without_overflowing() {
    let t = Timestamp::from_nanos(100);
    assert_eq!(
        t.checked_add(Duration::from_secs(30 * 60)),
        Some(Timestamp::from_nanos(1_800_000_000_100))
    );
    assert_eq!(
        Timestamp::from_nanos(u64::MAX).checked_add(Duration::from_nanos(1)),
        None
    );
    assert_eq!(t.checked_add(Duration::MAX), None);
}

#[test]
fn timestamps_subtract_durations_down_to_the_epoch() {
    let t = Timestamp::from_nanos(1_800_000_000_100);
    assert_eq!(
        t.checked_sub(Duration::from_secs(30 * 60)),
        Some(Timestamp::from_nanos(100))
    );
    assert_eq!(
        Timestamp::from_nanos(0).checked_sub(Duration::ZERO),
        Some(Timestamp::from_nanos(0))
    );
    assert_eq!(
        Timestamp::from_nanos(99).checked_sub(Duration::from_nanos(100)),
        None
    );
    assert_eq!(t.checked_sub(Duration::MAX), None);
}

#[test]
fn unix_seconds_add_whole_seconds_without_overflowing() {
    let s = UnixSeconds::new(1_800_000_000);
    assert_eq!(
        s.checked_add(Duration::from_secs(120)),
        Some(UnixSeconds::new(1_800_000_120))
    );
    assert_eq!(
        s.checked_add(Duration::from_millis(1_999)),
        Some(UnixSeconds::new(1_800_000_001))
    );
    assert_eq!(
        UnixSeconds::new(u64::MAX).checked_add(Duration::from_secs(1)),
        None
    );
}

#[test]
fn a_canonical_amount_stops_at_u128_max() {
    assert_eq!(
        TokenAmount::from_canonical_nat(Nat::from(u128::MAX)),
        Some(TokenAmount::from(u128::MAX))
    );
    assert_eq!(
        TokenAmount::from_canonical_nat(Nat::from(u128::MAX) + Nat::from(1_u8)),
        None
    );
}

/// The price of a transaction is the fee per gas multiplied by the gas it buys, computed on
/// the types that hold the units, and it answers `None` rather than wrapping.
#[test]
fn a_fee_per_gas_prices_the_gas_it_is_paid_for() {
    let fee = WeiPerGas::from(2_000_000_000_u64);
    assert_eq!(
        fee.transaction_cost(GasAmount::from(21_000_u32)),
        Some(Wei::from(42_000_000_000_000_u64))
    );
    assert_eq!(
        WeiPerGas::ZERO.transaction_cost(GasAmount::from(21_000_u32)),
        Some(Wei::ZERO)
    );
    assert_eq!(
        fee.transaction_cost(GasAmount::ZERO),
        Some(Wei::ZERO),
        "no gas costs nothing"
    );
    assert_eq!(WeiPerGas::MAX.transaction_cost(GasAmount::TWO), None);
}

/// How long ago an instant was, and nothing at all for an instant that is not past: a clock
/// that moved back reports no age rather than an age that underflows.
#[test]
fn an_instant_measures_how_long_ago_an_earlier_one_was() {
    let now = Timestamp::from_nanos(1_000_000_000);
    assert_eq!(
        now.saturating_duration_since(Timestamp::from_nanos(400_000_000)),
        Duration::from_millis(600)
    );
    assert_eq!(now.saturating_duration_since(now), Duration::ZERO);
    assert_eq!(
        now.saturating_duration_since(Timestamp::from_nanos(2_000_000_000)),
        Duration::ZERO
    );
}
