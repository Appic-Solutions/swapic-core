use crate::state::{pending_quotes, Store};
use crate::storage::halt::is_halted;
use crate::storage::{config, events};
use std::time::Duration;
use types::events::EventType;
use types::{Quote, QuoteHash, Swap, SwapStatus, Timestamp};

/// What one expiry pass did. Returned rather than logged, so the sweep is testable
/// without a canister and without reading the event log back.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Sweep {
    /// pending quotes dropped past their permit window
    pub dropped: usize,
    /// decision timeouts that became a `RefundStarted`
    pub refunds: usize,
    /// timed-out swaps the pass stepped over: `quote_bytes` that did not parse, or an
    /// append the guard refused. Counted, never fatal, because the sweep must be total.
    pub skipped: usize,
    /// whether either pass stopped at its cap with work still due, so the next tick has
    /// more to do. A pass is bounded; the timer is what makes it total.
    pub more: bool,
}

/// One pass of the expiry timer, bounded at both ends by the configured caps: an unbounded
/// pass would trap past the instruction budget, and a repeating timer retrying the same
/// trapping batch would disable the sweep for good. `now` is the caller's clock, so the
/// whole pass is testable without a canister.
pub fn run_expiry_sweep(now: Timestamp) -> Sweep {
    let config = config::get();
    let evicted = pending_quotes::sweep_expired(
        now.as_secs(),
        config.permit_deadline,
        config.max_evictions_per_sweep.as_usize(),
    );
    let mut swept = Sweep {
        dropped: evicted.dropped,
        more: evicted.more,
        ..Sweep::default()
    };
    // a halted canister's log and state disagree, so it appends nothing: dropping a stale
    // quote is pre-money hygiene, starting a refund is money
    if is_halted() {
        return swept;
    }
    let (timed_out, more_waiting) = timed_out_waiting(
        now,
        config.decision_timeout,
        config.max_refunds_per_sweep.as_usize(),
    );
    swept.more |= more_waiting;
    let (due, unreadable) = due_refunds(timed_out, now, config.decision_timeout);
    swept.skipped = unreadable;
    // decided first, then appended, so every refund of the pass is judged against the same
    // fold and one append cannot change which swaps the pass sees
    for quote_hash in due {
        let appended = events::append_event_at(
            EventType::RefundStarted {
                quote_hash,
                reason: "decision timeout".to_string(),
            },
            now,
        );
        match appended {
            Ok(_) => swept.refunds += 1,
            // the guard refusing one swap is not a reason to abandon the others
            Err(_) => swept.skipped += 1,
        }
    }
    swept
}

/// The `cap` longest-waiting swaps whose wait began more than `timeout` before `now`, and
/// whether a further one was waiting behind them. Read off the waiting index rather than a
/// scan of every swap, so a pass costs what it takes, not what ever settled.
fn timed_out_waiting(
    now: Timestamp,
    timeout: Duration,
    cap: usize,
) -> (Vec<(QuoteHash, Swap)>, bool) {
    // a clock less than a whole timeout past the epoch has no wait that old
    let Some(cutoff) = now.checked_sub(timeout) else {
        return (Vec::new(), false);
    };
    events::read_state(|state| {
        let store = state.store();
        // one key past the cap: the extra key is the evidence that work remains, and the
        // index is ordered by wait, so the keys taken are the oldest waits there are
        let mut keys = store.waiting_since_before(cutoff, cap.saturating_add(1));
        let more = keys.len() > cap;
        keys.truncate(cap);
        // an index entry always has its swap: the same record step writes both
        let swaps = keys
            .into_iter()
            .filter_map(|quote_hash| store.swap(&quote_hash).map(|swap| (quote_hash, swap)))
            .collect();
        (swaps, more)
    })
}

/// Which waiting swaps have run out of time and whose quote asked for an automatic refund,
/// plus a count of the ones whose `quote_bytes` did not parse. Pure and total: an
/// unreadable quote is counted and stepped over, never a panic. The sweep hands it the
/// index's timed-out entries only; the policy holds for whatever it is handed.
fn due_refunds(
    swaps: impl IntoIterator<Item = (QuoteHash, Swap)>,
    now: Timestamp,
    timeout: Duration,
) -> (Vec<QuoteHash>, usize) {
    let mut due = Vec::new();
    let mut unreadable = 0;
    for (quote_hash, swap) in swaps {
        if swap.status != SwapStatus::WaitingForUser {
            continue;
        }
        let Some(since) = swap.waiting_since else {
            continue;
        };
        // older than the timeout, so the deadline itself is still the user's; a deadline
        // past what a timestamp can hold never comes
        if since
            .checked_add(timeout)
            .is_none_or(|deadline| now <= deadline)
        {
            continue;
        }
        match Quote::parse(&swap.quote_bytes) {
            Ok(quote) if quote.auto_refund => due.push(quote_hash),
            // the other half of the dual refund policy: this user asked to be consulted,
            // so the swap keeps waiting however long that takes
            Ok(_) => {}
            Err(_) => unreadable += 1,
        }
    }
    (due, unreadable)
}

#[cfg(test)]
mod tests;
