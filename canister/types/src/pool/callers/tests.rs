use super::*;
use std::time::Duration;

fn at(secs: u64) -> Timestamp {
    Timestamp::from_secs(secs).unwrap()
}

/// Lets `n` calls through at `now`, one after another.
fn calls(mut record: Option<CallerCalls>, n: usize, now: Timestamp) -> Option<CallerCalls> {
    for _ in 0..n {
        record = Some(admit(record, now).unwrap());
    }
    record
}

#[test]
fn five_calls_a_minute_pass_and_no_more() {
    let record = calls(None, usize::from(PER_MINUTE), at(100));
    assert_eq!(admit(record, at(159)), Err(CallRefusal::PerMinute));
    // a minute after its first call, its minute starts again
    assert!(admit(record, at(160)).is_ok());
}

#[test]
fn thirty_calls_an_hour_pass_and_no_more() {
    let mut record = None;
    for minute in 0..6 {
        record = calls(record, usize::from(PER_MINUTE), at(100 + 60 * minute));
    }
    assert_eq!(record.unwrap().in_hour, PER_HOUR);
    assert_eq!(admit(record, at(100 + 60 * 6)), Err(CallRefusal::PerHour));
    assert_eq!(admit(record, at(100 + 3_599)), Err(CallRefusal::PerHour));
    // an hour after its first call, its hour starts again
    let again = admit(record, at(100 + 3_600)).unwrap();
    assert_eq!((again.in_minute, again.in_hour), (1, 1));
}

/// A refused call counts for nothing: the record it would change is not answered.
#[test]
fn a_refused_call_counts_for_nothing() {
    let record = calls(None, usize::from(PER_MINUTE), at(100));
    for _ in 0..10 {
        assert!(admit(record, at(101)).is_err());
    }
    assert_eq!(record.unwrap().in_minute, PER_MINUTE);
    let _ = Duration::ZERO;
}
