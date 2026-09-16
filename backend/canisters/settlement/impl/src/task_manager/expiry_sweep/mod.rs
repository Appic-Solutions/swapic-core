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
}

/// One pass of the expiry timer. `now` is the caller's clock, so the whole pass is
/// testable without a canister.
pub fn run_expiry_sweep(now: Timestamp) -> Sweep {
    let config = config::get();
    let mut swept = Sweep {
        dropped: pending_quotes::sweep_expired(now.as_secs(), config.permit_deadline),
        ..Sweep::default()
    };
    // a halted canister's log and state disagree, so it appends nothing: dropping a stale
    // quote is pre-money hygiene, starting a refund is money
    if is_halted() {
        return swept;
    }
    let swaps = events::read_state(|state| state.store().swaps());
    let (due, unreadable) = due_refunds(swaps, now, config.decision_timeout);
    swept.skipped = unreadable;
    // collected first, then appended: `read_state` holds a shared borrow of the state that
    // `append_event` takes mutably, so appending inside that closure would panic
    for quote_hash in due {
        let appended = events::append_event(EventType::RefundStarted {
            quote_hash,
            reason: "decision timeout".to_string(),
        });
        match appended {
            Ok(_) => swept.refunds += 1,
            // the guard refusing one swap is not a reason to abandon the others
            Err(_) => swept.skipped += 1,
        }
    }
    swept
}

/// Which waiting swaps have run out of time and whose quote asked for an automatic refund,
/// plus a count of the ones whose `quote_bytes` did not parse. Pure and total: an
/// unreadable quote is counted and stepped over, never a panic.
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
