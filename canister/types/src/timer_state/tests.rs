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

/// Reads every 30 seconds over `from..=to`, each giving `read`: how many alerted.
fn reads(seen: &mut FinalizedSeen, from: u64, to: u64, read: Option<BlockNumber>) -> usize {
    (from..=to)
        .step_by(30)
        .filter(|secs| seen.observe(read, at(*secs)))
        .count()
}

/// The stall clock runs from the last read that moved the finalized block, while something
/// waits: a read that gives the same number or fails leaves it, a stall raises one alert,
/// and a move ends it.
#[test]
fn a_finalized_block_standing_still_ten_minutes_alerts_once() {
    let block = BlockNumber::new;
    let mut seen = FinalizedSeen::first(at(0));
    // a provider that never serves the tag stalls too
    assert_eq!(reads(&mut seen, 0, 570, None), 0);
    assert!(
        seen.observe(None, at(600)),
        "ten minutes from the first try"
    );
    assert_eq!(reads(&mut seen, 630, 690, None), 0, "one alert per stall");
    assert!(!seen.observe(Some(block(50)), at(710)), "a move ends it");
    assert!(!seen.is_stalled(at(720)));
    assert_eq!(
        reads(&mut seen, 720, 1_290, Some(block(50))),
        0,
        "the same number moves nothing"
    );
    assert!(seen.observe(Some(block(50)), at(1_310)));
    assert!(
        !seen.observe(Some(block(49)), at(1_320)),
        "nor does an older one"
    );
    assert!(seen.is_stalled(at(1_330)));
    assert!(!seen.observe(Some(block(51)), at(1_340)));
    assert!(!seen.is_stalled(at(1_340)));
}

/// Review fix: the stall clock runs only while something waits. A chain idle past a minute
/// is not stalled, however long ago its block moved, and the first read after the idle
/// starts the clock anew instead of raising the alert at once.
#[test]
fn an_idle_chain_is_not_stalled_and_its_clock_starts_anew() {
    let mut seen = FinalizedSeen::first(at(0));
    assert!(!seen.observe(Some(BlockNumber::new(50)), at(0)));
    assert!(!seen.is_stalled(at(3_600)), "an hour with nothing waiting");
    // waiting again, the first read failing: no alert, the clock starts at that read
    assert!(!seen.observe(None, at(3_600)));
    assert_eq!(seen.moved_at, at(3_600));
    assert!(!seen.is_stalled(at(3_630)));
    // failing ten minutes on: the stall's one alert
    assert_eq!(reads(&mut seen, 3_630, 4_200, None), 1);
    assert!(seen.is_stalled(at(4_200)));
    assert!(!seen.is_stalled(at(4_300)), "idle again");
}

/// Review fix: the finalized block is read at most every six seconds; between reads the
/// last number read stands.
#[test]
fn the_finalized_block_is_read_at_its_own_pace() {
    let mut seen = FinalizedSeen::first(at(0));
    assert!(seen.read_due(at(0)), "never read");
    seen.observe(Some(BlockNumber::new(50)), at(0));
    assert!(!seen.read_due(at(2)));
    assert!(!seen.read_due(at(5)));
    assert!(seen.read_due(at(6)));
    // a failed read counts as a try: the next waits its pace too
    seen.observe(None, at(6));
    assert!(!seen.read_due(at(8)));
    assert_eq!(seen.number, Some(BlockNumber::new(50)));
}
