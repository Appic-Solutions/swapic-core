use super::*;

fn at(secs: u64) -> Timestamp {
    Timestamp::from_secs(1_800_000_000 + secs).unwrap()
}

fn tx(byte: u8) -> TxHash {
    TxHash::new([byte; 32])
}

#[test]
fn ten_reports_a_minute_in_total_then_none_until_the_minute_passes() {
    let mut limiter = Limiter::default();
    for byte in 0..10 {
        assert_eq!(limiter.admit(tx(byte), at(u64::from(byte))), Ok(()));
    }
    assert!(limiter.admit(tx(20), at(30)).is_err());
    // the first went out of the minute: one more
    assert_eq!(limiter.admit(tx(20), at(60)), Ok(()));
    assert!(limiter.admit(tx(21), at(60)).is_err());
}

#[test]
fn three_attempts_an_hour_at_one_transaction() {
    let mut limiter = Limiter::default();
    for minute in 0..3 {
        assert_eq!(limiter.admit(tx(1), at(minute * 60)), Ok(()));
    }
    assert!(limiter.admit(tx(1), at(600)).is_err());
    // another transaction is not held by it
    assert_eq!(limiter.admit(tx(2), at(600)), Ok(()));
    // an hour after the first, one more
    assert_eq!(limiter.admit(tx(1), at(3_600)), Ok(()));
}

#[test]
fn a_refused_report_counts_for_nothing() {
    let mut limiter = Limiter::default();
    for minute in 0..3 {
        limiter.admit(tx(1), at(minute * 60)).unwrap();
    }
    for _ in 0..20 {
        assert!(limiter.admit(tx(1), at(300)).is_err());
    }
    // the refusals took nothing of the minute's ten
    for byte in 2..12 {
        assert_eq!(limiter.admit(tx(byte), at(300)), Ok(()));
    }
}
