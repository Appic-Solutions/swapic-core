use super::*;

fn reading(block: u64) -> ChainReading {
    ChainReading {
        block: BlockNumber::new(block),
        base_fee: WeiPerGas::from(1_000_000_000_u64),
        priority_fee: WeiPerGas::from(100_000_000_u64),
    }
}

fn at(secs: u64) -> Timestamp {
    Timestamp::from_secs(secs).expect("a test instant is inside the epoch")
}

/// The watcher hands over a reading; the canister says when it arrived. Nothing the caller
/// sends can move that instant, because the reading has no field for it.
#[test]
fn a_reading_carries_no_instant_until_the_canister_stamps_one() {
    let data = reading(19_000_000).pushed_at(at(1_700_000_000));
    assert_eq!(data.block, BlockNumber::new(19_000_000));
    assert_eq!(data.base_fee, WeiPerGas::from(1_000_000_000_u64));
    assert_eq!(data.priority_fee, WeiPerGas::from(100_000_000_u64));
    assert_eq!(data.pushed_at, at(1_700_000_000));
}

/// The cap is inclusive: data exactly `max_age` old is still the data the canister decides
/// on, and one nanosecond past it is not.
#[test]
fn data_is_fresh_until_its_age_passes_the_cap() {
    let max_age = Duration::from_secs(10);
    let data = reading(1).pushed_at(at(1_000));
    assert!(data.is_fresh(at(1_000), max_age), "no age at all");
    assert!(data.is_fresh(at(1_010), max_age), "exactly the cap");
    assert!(
        !data.is_fresh(Timestamp::from_nanos(at(1_010).as_nanos() + 1), max_age),
        "one nanosecond past the cap"
    );
    assert!(!data.is_fresh(at(2_000), max_age), "long past the cap");
}

/// A stamp ahead of the clock is a clock that moved back, not data from the future: it has
/// no age, so it is fresh rather than negative.
#[test]
fn data_stamped_ahead_of_the_clock_has_no_age() {
    let data = reading(1).pushed_at(at(2_000));
    assert!(data.is_fresh(at(1_000), Duration::from_secs(10)));
}

/// A zero cap is what a config sets to mean "only data pushed this instant", and it holds:
/// the same instant passes, anything older does not.
#[test]
fn a_zero_cap_admits_only_the_instant_itself() {
    let data = reading(1).pushed_at(at(1_000));
    assert!(data.is_fresh(at(1_000), Duration::ZERO));
    assert!(!data.is_fresh(
        Timestamp::from_nanos(at(1_000).as_nanos() + 1),
        Duration::ZERO
    ));
}

/// A stored reading survives the stable map it lives in.
#[test]
fn chain_data_round_trips_through_storage() {
    use ic_stable_structures::Storable;
    let data = reading(19_000_000).pushed_at(at(1_700_000_000));
    assert_eq!(ChainData::from_bytes(data.to_bytes()), data);
}

/// A ceiling capped from below lowers both fields to the cap, and the tip stays inside the
/// ceiling it is paid out of; a cap above the ceiling changes nothing. What lets a bound on
/// the whole bill, which a large gas limit turns into a low price per unit, take the place
/// of the fee ceiling in a replacement.
#[test]
fn a_ceiling_capped_below_itself_lowers_both_fields() {
    let gwei = |n: u64| WeiPerGas::from(n * 1_000_000_000);
    let ceiling = reading(1).pushed_at(at(1)).fee_ceiling();
    assert_eq!(
        ceiling.max_fee(),
        gwei(8)
            .checked_add(WeiPerGas::from(800_000_000_u64))
            .unwrap()
    );

    let capped = ceiling.capped(gwei(5));
    assert_eq!(capped.max_fee(), gwei(5));
    assert_eq!(capped.max_priority_fee(), gwei(5));
    assert_eq!(
        ceiling.capped(gwei(10)),
        ceiling,
        "a cap above the ceiling is no cap"
    );

    let fees = Fees::new(gwei(4), gwei(1)).unwrap();
    assert_eq!(
        fees.capped(gwei(2)),
        Fees::new(gwei(2), gwei(1)).unwrap(),
        "a tip already under the cap is left alone"
    );
    assert_eq!(
        fees.capped(WeiPerGas::ZERO),
        Fees::new(WeiPerGas::ZERO, WeiPerGas::ZERO).unwrap(),
        "and the tip never ends up above the ceiling"
    );
}

/// A replacement is never priced under what the chain is asking now. When the ceiling a
/// cost bound turned into sits below the floor, no bid can be both affordable and
/// acceptable, and the answer is no bid rather than a signature on bytes the chain will
/// not mine.
#[test]
fn no_replacement_is_bid_below_the_floor() {
    let gwei = |n: u64| WeiPerGas::from(n * 1_000_000_000);
    let old = Fees::new(gwei(1), gwei(1)).unwrap();
    let floor = Fees::new(gwei(3), gwei(1)).unwrap();
    let ceiling_under_floor = Fees::new(gwei(2), gwei(1)).unwrap();
    assert_eq!(old.bumped(floor, ceiling_under_floor), None);

    let ceiling_at_floor = floor;
    assert_eq!(
        old.bumped(floor, ceiling_at_floor),
        Some(floor),
        "a ceiling at the floor still allows the one bid the chain asks for"
    );
}
