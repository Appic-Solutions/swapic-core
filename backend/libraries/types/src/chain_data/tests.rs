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
