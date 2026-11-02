use super::*;

fn at(secs: u64) -> Timestamp {
    Timestamp::from_secs(1_800_000_000 + secs).unwrap()
}

fn tx(byte: u8) -> TxHash {
    TxHash::new([byte; 32])
}

/// Review of the merged canister (item 2): every report costs a read all the nodes make, so
/// the callers other than the watcher, together, are held to two a minute and twenty an hour.
#[test]
fn two_reports_a_minute_and_twenty_an_hour_in_total() {
    let mut limiter = Limiter::default();
    assert_eq!(limiter.admit(tx(0), at(0)), Ok(()));
    assert_eq!(limiter.admit(tx(1), at(1)), Ok(()));
    assert!(
        limiter.admit(tx(2), at(30)).is_err(),
        "a third in the minute"
    );
    // a minute on: two more, and so on until twenty in the hour
    for minute in 1..10 {
        for byte in 0..2 {
            let n = 2 * minute + byte;
            assert_eq!(limiter.admit(tx(n as u8), at(60 * minute + byte)), Ok(()));
        }
    }
    assert!(
        limiter.admit(tx(99), at(60 * 10)).is_err(),
        "the twenty-first in the hour"
    );
    // an hour after the first, room again
    assert_eq!(limiter.admit(tx(99), at(3_600)), Ok(()));
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
    // the refusals took nothing of the minute's two
    for byte in 2..4 {
        assert_eq!(limiter.admit(tx(byte), at(300)), Ok(()));
    }
}

/// The read of Base stands at a block or at a log inside one: a block done whole is past
/// every log of it, and a later block past both.
#[test]
fn a_cursor_orders_by_block_then_log() {
    let at = |block: u64, log: Option<u64>| ScanCursor {
        block: crate::numeric::BlockNumber::new(block),
        log,
    };
    assert!(at(10, Some(3)).position() < at(10, Some(4)).position());
    assert!(at(10, Some(u64::MAX)).position() < at(10, None).position());
    assert!(at(10, None).position() < at(11, Some(0)).position());
}
