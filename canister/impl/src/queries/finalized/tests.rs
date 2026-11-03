use super::*;
use types::{BlockNumber, ChainId};

fn at(secs: u64) -> Timestamp {
    Timestamp::from_secs(1_800_000_000 + secs).unwrap()
}

/// Review fix: a chain whose block stood still past ten minutes is stalled only while
/// something on it waits (it is in the outbox's receipt schedule); idle, it is not.
#[test]
fn an_idle_chain_is_never_reported_stalled() {
    let mut state = TimerState::default();
    let mut seen = types::timer_state::FinalizedSeen::first(at(0));
    seen.observe(Some(BlockNumber::new(50)), at(0));
    state.finalized_seen.insert(ChainId::POLYGON, seen);
    let stalled = |state: &TimerState| seen_at(state, at(3_600))[0].1.stalled;
    assert!(!stalled(&state), "idle");
    state.receipts_due.insert(ChainId::POLYGON, at(3_602));
    assert!(stalled(&state), "waiting");
}
