use super::*;
use crate::storage::on_fresh_memory;
use types::pool::callers::{CallerCalls, PER_MINUTE};

fn caller(byte: u8) -> Principal {
    Principal::from_slice(&[byte; 29])
}

fn at(secs: u64) -> Timestamp {
    Timestamp::from_secs(secs).unwrap()
}

/// A caller past its limit is refused with nothing written, its record included; another
/// caller is counted apart.
#[test]
fn a_caller_past_its_limit_is_refused_with_nothing_written() {
    on_fresh_memory(|| {
        crate::storage::init();
        for _ in 0..PER_MINUTE {
            admit(caller(1), at(100)).unwrap();
        }
        let record: Option<CallerCalls> = crate::storage::callers::get(caller(1));
        assert!(matches!(
            admit(caller(1), at(110)),
            Err(PoolError::Unavailable(why)) if why.contains("a minute")
        ));
        assert_eq!(crate::storage::callers::get(caller(1)), record);
        assert!(admit(caller(2), at(110)).is_ok());
        assert_eq!(crate::storage::callers::len(), 2);
        // its minute over, the caller passes again
        assert!(admit(caller(1), at(160)).is_ok());
    });
}
