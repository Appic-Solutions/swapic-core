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
            ((), true)
        });
        let kept = get();
        assert_eq!(kept.reports.recent, vec![now]);
        assert_eq!(kept.unfilled_reads.get(&7), Some(&now));
        // the guard counts what the cell holds: the minute's two are spent across calls
        let admit = |byte: u8| {
            update(|state| {
                let admitted = state.reports.admit(TxHash::new([byte; 32]), now);
                let changed = admitted.is_ok();
                (admitted, changed)
            })
        };
        admit(2).unwrap();
        assert!(admit(20).is_err());
    });
}

/// The last review (item 6): the closure says whether it changed the state, and only then is
/// the cell written: no clone and no comparison of the whole state on every call.
#[test]
fn the_cell_is_written_only_when_the_closure_says_it_changed() {
    on_fresh_memory(|| {
        crate::storage::init();
        WRITES.with(|writes| writes.set(0));
        let seen = update(|state| (state.alerted_head.is_some(), false));
        assert!(!seen);
        assert_eq!(WRITES.with(|writes| writes.get()), 0, "nothing changed");
        update(|state| {
            state.unfilled_reads.insert(1, Timestamp::from_nanos(1));
            ((), true)
        });
        assert_eq!(WRITES.with(|writes| writes.get()), 1);
        assert_eq!(
            get().unfilled_reads.get(&1),
            Some(&Timestamp::from_nanos(1))
        );
    });
}
