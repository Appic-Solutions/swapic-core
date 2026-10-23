use super::*;
use crate::storage::on_fresh_memory;
use types::{Timestamp, TxHash};

/// C-09: what the doors and timers remember is in the stable cell, read back as written:
/// what an upgrade keeps.
#[test]
fn the_timers_memory_is_kept_in_stable_memory() {
    on_fresh_memory(|| {
        crate::storage::init();
        assert_eq!(get(), TimerState::default());
        let now = Timestamp::from_secs(1_800_000_000).unwrap();
        update(|state| {
            state.reports.admit(TxHash::new([1; 32]), now).unwrap();
            state.unfilled_reads.insert(7, now);
        });
        let kept = get();
        assert_eq!(kept.reports.recent, vec![now]);
        assert_eq!(kept.unfilled_reads.get(&7), Some(&now));
        // the guard counts what the cell holds: the minute's two are spent across calls
        update(|state| state.reports.admit(TxHash::new([2; 32]), now)).unwrap();
        assert!(update(|state| state.reports.admit(TxHash::new([20; 32]), now)).is_err());
    });
}

/// Review of the merged canister (item 6): the cell is written only when the state changed.
#[test]
fn the_cell_is_written_only_when_the_state_changes() {
    on_fresh_memory(|| {
        crate::storage::init();
        WRITES.with(|writes| writes.set(0));
        update(|state| state.alerted_head.is_some());
        assert_eq!(WRITES.with(|writes| writes.get()), 0, "nothing changed");
        update(|state| state.unfilled_reads.insert(1, Timestamp::from_nanos(1)));
        assert_eq!(WRITES.with(|writes| writes.get()), 1);
    });
}
