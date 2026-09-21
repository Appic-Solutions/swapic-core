use crate::state::{pending_quotes, Store};
use crate::storage::halt::is_halted;
use crate::storage::{config, events};
use std::collections::BTreeSet;
use std::time::Duration;
use types::events::EventType;
use types::{QuoteHash, Swap, Timestamp, WaitingKey};

/// What one expiry pass did. Returned rather than logged, so the sweep is testable
/// without a canister and without reading the event log back.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Sweep {
    /// pending quotes dropped past their permit window
    pub dropped: usize,
    /// decision timeouts that became a `RefundStarted`
    pub refunds: usize,
    /// appends the pass could not make: a refund or a repair the log refused. Counted,
    /// never fatal, because the sweep must be total.
    pub skipped: usize,
    /// index entries the pass repaired through a logged `WaitingRepaired`, because the swap
    /// is not in the wait the entry says: no such swap, or not waiting, or waiting for a
    /// human, or waiting since another instant. Only a fold no event could have produced
    /// has any, and the pass tidies rather than trips. An entry whose repair the log refused
    /// stays in the index for the next pass and is counted in `skipped`.
    pub stale: usize,
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
    let head = timed_out_waiting(
        now,
        config.decision_timeout,
        config.max_refunds_per_sweep.as_usize(),
    );
    swept.more |= head.more;
    // the index is the queue, so an entry that is not work the timer can do is repaired
    // rather than stepped over: left in place it would hold a slot of every pass's cap for
    // good, and a cap's worth of them would starve every refund behind them.
    // The drop is a logged event like every other write to the fold, so the repair is in the
    // record the deep audit compares against instead of quietly erasing what it would find
    // one repair per swap: it makes every entry of the swap right, so a second line for the
    // same swap would change nothing and still sit on the log and in the cap
    let mut repaired_swaps = BTreeSet::new();
    for key in &head.stale {
        if !repaired_swaps.insert(key.quote_hash) {
            continue;
        }
        let repaired = events::append_event_at(
            EventType::WaitingRepaired {
                quote_hash: key.quote_hash,
            },
            now,
        );
        match repaired {
            Ok(_) => swept.stale += 1,
            // an entry the log refuses to repair stays for the next pass, like a refund
            Err(_) => swept.skipped += 1,
        }
    }
    // decided first, then appended, so every refund of the pass is judged against the same
    // fold and one append cannot change which swaps the pass sees
    for quote_hash in due_refunds(head.waiting, now, config.decision_timeout) {
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

/// What one pass found at the head of the waiting index.
#[derive(Clone, Debug, Default, PartialEq)]
struct Head {
    /// entries that are the wait their swap is in: waiting for its user, asking for an
    /// automatic refund, since the instant the key says. Oldest wait first
    waiting: Vec<(QuoteHash, Swap)>,
    /// entries that are not: the swap stopped waiting or never did, waits for a human,
    /// waits since another instant, or is no swap at all
    stale: Vec<WaitingKey>,
    /// whether a further timed-out entry sat behind the cap
    more: bool,
}

/// The `cap` oldest indexed waits that began more than `timeout` before `now`, split into the
/// ones that are still work and the ones that are not. Read off the index rather than a scan
/// of every swap, so a pass costs what it takes, not what ever settled.
fn timed_out_waiting(now: Timestamp, timeout: Duration, cap: usize) -> Head {
    // a clock less than a whole timeout past the epoch has no wait that old
    let Some(cutoff) = now.checked_sub(timeout) else {
        return Head::default();
    };
    events::read_state(|state| {
        let store = state.store();
        // one key past the cap: the extra key is the evidence that work remains, and the
        // index is ordered by wait, so the keys taken are the oldest waits there are
        let mut keys = store.auto_refund_waiting_since_before(cutoff, cap.saturating_add(1));
        let more = keys.len() > cap;
        keys.truncate(cap);
        let mut head = Head {
            more,
            ..Head::default()
        };
        for key in keys {
            // the one record step that indexes a swap writes the key its wait implies, and
            // every step that stops the wait removes it, so only a divergence lands in the
            // stale half: an entry that differs from the swap's own wait in any way
            match store.swap(&key.quote_hash) {
                Some(swap) if swap.auto_refund_wait() == Some(key.since) => {
                    head.waiting.push((key.quote_hash, swap))
                }
                _ => head.stale.push(key),
            }
        }
        head
    })
}

/// Which of the swaps have run out of time: those whose own wait, the one the index holds
/// for them, began more than `timeout` before `now`. Pure and total: "older than the
/// timeout" is strict, so the deadline itself is still the user's, a deadline past what a
/// timestamp can hold never comes, and a swap with no wait the timer can act on is not due.
/// The sweep hands it the head of the index only; the rule holds for whatever it is handed.
fn due_refunds(
    swaps: impl IntoIterator<Item = (QuoteHash, Swap)>,
    now: Timestamp,
    timeout: Duration,
) -> Vec<QuoteHash> {
    swaps
        .into_iter()
        .filter_map(|(quote_hash, swap)| {
            let since = swap.auto_refund_wait()?;
            since
                .checked_add(timeout)
                .is_some_and(|deadline| now > deadline)
                .then_some(quote_hash)
        })
        .collect()
}

#[cfg(test)]
mod tests;
