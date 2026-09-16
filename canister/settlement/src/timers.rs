use crate::events::{Event, Hash32};
use crate::log::{self, Memory, HALT_MEMORY};
use crate::state::{AppState, SwapStatus};
use crate::{config, quote};
use ic_cdk_timers::TimerId;
use ic_stable_structures::StableCell;
use std::cell::RefCell;
use std::time::Duration;

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

thread_local! {
    // The halt flag: stable, because a halted canister must stay halted across the upgrade
    // that an operator reaches for first. There is no heap cache to drift from it.
    static HALTED: RefCell<StableCell<bool, Memory>> = RefCell::new(
        StableCell::init(log::memory(HALT_MEMORY), false).expect("halt cell init"),
    );

    // The live timers, so `start_timers` can replace them instead of doubling them.
    static TIMERS: RefCell<Vec<TimerId>> = const { RefCell::new(Vec::new()) };
}

pub fn is_halted() -> bool {
    HALTED.with(|h| *h.borrow().get())
}

/// Storage path; the caller does the authorization. True is an emergency stop, false is a
/// human saying the divergence the audit found has been explained.
pub fn set_halted(halted: bool) {
    // out of stable memory is not a caller error, so it traps rather than returning
    HALTED.with(|h| h.borrow_mut().set(halted).expect("halt cell write"));
}

/// The gate Plan 3's money endpoints call before they move anything.
pub fn require_not_halted() -> Result<(), String> {
    if is_halted() {
        Err("canister is halted: a replay audit found the log and the state disagree".to_string())
    } else {
        Ok(())
    }
}

/// A zero interval is a repeating timer with no gap between runs, which burns cycles for
/// nothing, so every configured interval is clamped to at least a second.
fn interval(seconds: u64) -> Duration {
    Duration::from_secs(seconds.max(1))
}

/// Wires both timers. Called from `init` and `post_upgrade`, because timers live in the
/// heap and an upgrade clears them, and from `set_config`, so new intervals apply at once.
pub fn start_timers() {
    // touch the halt cell here, in an update context, so no query is ever the first to
    // grow its stable memory
    let _ = is_halted();
    let config = config::get();
    // idempotent: a second call replaces the timers rather than doubling the sweep rate
    TIMERS.with(|t| {
        for id in t.borrow_mut().drain(..) {
            ic_cdk_timers::clear_timer(id);
        }
    });
    let expiry =
        ic_cdk_timers::set_timer_interval(interval(config.expiry_check_interval_s), || {
            // seconds, to match the quote expiries the sweep compares against
            run_expiry_sweep(ic_cdk::api::time() / 1_000_000_000);
        });
    let audit = ic_cdk_timers::set_timer_interval(
        interval(config.replay_audit_interval_s),
        run_replay_audit,
    );
    TIMERS.with(|t| *t.borrow_mut() = vec![expiry, audit]);
}

/// One pass of the expiry timer. `now_s` is the caller's clock, so the whole pass is
/// testable without a canister.
pub fn run_expiry_sweep(now_s: u64) -> Sweep {
    let config = config::get();
    let mut swept = Sweep {
        dropped: quote::sweep_expired(now_s, config.permit_deadline_s),
        ..Sweep::default()
    };
    // a halted canister's log and state disagree, so it appends nothing: dropping a stale
    // quote is pre-money hygiene, starting a refund is money
    if is_halted() {
        return swept;
    }
    let timeout_ns = config
        .decision_timeout_min
        .saturating_mul(60)
        .saturating_mul(1_000_000_000);
    let (due, unreadable) = log::with_state(|state| {
        due_refunds(state, now_s.saturating_mul(1_000_000_000), timeout_ns)
    });
    swept.skipped = unreadable;
    // collected first, then appended: `with_state` holds a shared borrow of the state that
    // `append_event` takes mutably, so appending inside that closure would panic
    for quote_hash in due {
        let appended = log::append_event(Event::RefundStarted {
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

/// One pass of the replay audit: the chain must link from genesis and folding the log must
/// reproduce the live state.
pub fn run_replay_audit() {
    record_audit(log::verify_chain() && log::verify_replay());
}

/// One audit's verdict. A pass never clears the flag: only a human does, through
/// `set_halted`, once the divergence is understood.
fn record_audit(ok: bool) {
    if !ok {
        set_halted(true);
    }
}

/// Which waiting swaps have run out of time and whose quote asked for an automatic refund,
/// plus a count of the ones whose `quote_bytes` did not parse. Pure and total: an
/// unreadable quote is counted and stepped over, never a panic.
fn due_refunds(state: &AppState, now_ns: u64, timeout_ns: u64) -> (Vec<Hash32>, usize) {
    let mut due = Vec::new();
    let mut unreadable = 0;
    for (quote_hash, swap) in &state.swaps {
        if swap.status != SwapStatus::WaitingForUser {
            continue;
        }
        let Some(since_ns) = swap.waiting_since_ns else {
            continue;
        };
        // older than the timeout, so the deadline second itself is still the user's
        if now_ns <= since_ns.saturating_add(timeout_ns) {
            continue;
        }
        match quote::parse_quote(&swap.quote_bytes) {
            Ok(quote) if quote.auto_refund => due.push(*quote_hash),
            // the other half of the dual refund policy: this user asked to be consulted,
            // so the swap keeps waiting however long that takes
            Ok(_) => {}
            Err(_) => unreadable += 1,
        }
    }
    (due, unreadable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quote::{quote_bytes, GasMode, Quote};
    use crate::state::SwapState;

    fn quote(auto_refund: bool, nonce: u64) -> Quote {
        Quote {
            version: 1,
            src_chain: 8453,
            src_token: "usdc".into(),
            amount_in: 25_000_000,
            dst_chain: 42161,
            dst_token: "usdc".into(),
            expected_out: 24_990_000,
            min_out: 24_900_000,
            dst_address: "0xuser".into(),
            refund_address: None,
            auto_refund,
            gas_mode: GasMode::Gasless,
            rail: "cctp_v2_fast".into(),
            expires_at_s: 1_800_000_000,
            nonce,
        }
    }

    /// A swap parked on a user decision since `since_ns`, carrying `bytes` as the quote the
    /// sweep will read `auto_refund` out of.
    fn waiting(bytes: Vec<u8>, since_ns: u64) -> SwapState {
        SwapState {
            quote_bytes: bytes,
            status: SwapStatus::WaitingForUser,
            attempts: 0,
            open_attempt: None,
            src_chain: 8453,
            src_token: "usdc".into(),
            amount_in: 25_000_000,
            amount_paid: 0,
            waiting_since_ns: Some(since_ns),
        }
    }

    fn state_of(swaps: Vec<(Hash32, SwapState)>) -> AppState {
        AppState {
            swaps: swaps.into_iter().collect(),
            ..AppState::default()
        }
    }

    const MINUTE_NS: u64 = 60 * 1_000_000_000;

    #[test]
    fn interval_never_falls_below_a_second() {
        assert_eq!(interval(0), Duration::from_secs(1), "a zero interval spins");
        assert_eq!(interval(1), Duration::from_secs(1));
        assert_eq!(interval(21_600), Duration::from_secs(21_600));
    }

    /// The dual refund policy, at the point the timer decides: only a swap whose quote says
    /// `auto_refund` is refunded on its own. The others keep waiting for a human.
    #[test]
    fn due_refunds_picks_the_timed_out_auto_refund_swaps_only() {
        let auto = quote_bytes(&quote(true, 1));
        let manual = quote_bytes(&quote(false, 2));
        let state = state_of(vec![
            ([1; 32], waiting(auto.clone(), 0)),
            ([2; 32], waiting(manual, 0)),
            // asked for a refund, but not yet timed out
            ([3; 32], waiting(auto.clone(), 25 * MINUTE_NS)),
            // not waiting for anyone
            (
                [4; 32],
                SwapState {
                    status: SwapStatus::Executing,
                    waiting_since_ns: None,
                    ..waiting(auto, 0)
                },
            ),
        ]);
        let (due, skipped) = due_refunds(&state, 30 * MINUTE_NS + 1, 30 * MINUTE_NS);
        assert_eq!(due, vec![[1; 32]]);
        assert_eq!(skipped, 0);
    }

    /// "Older than the timeout" is strict: the deadline second itself is still the user's.
    #[test]
    fn due_refunds_fires_the_moment_after_the_timeout_and_not_before() {
        let state = state_of(vec![([1; 32], waiting(quote_bytes(&quote(true, 1)), 100))]);
        let timeout = 30 * MINUTE_NS;
        assert!(due_refunds(&state, 100 + timeout, timeout).0.is_empty());
        assert_eq!(
            due_refunds(&state, 100 + timeout + 1, timeout).0,
            vec![[1; 32]]
        );
    }

    /// `quote_bytes` reaches the log from a caller, so the sweep has to survive bytes it
    /// cannot read. It counts them and moves on rather than trapping the whole pass.
    #[test]
    fn due_refunds_is_total_when_quote_bytes_do_not_parse() {
        let state = state_of(vec![
            ([1; 32], waiting(vec![], 0)),
            ([2; 32], waiting(vec![0xff; 9], 0)),
            ([3; 32], waiting(quote_bytes(&quote(true, 1)), 0)),
        ]);
        let (due, skipped) = due_refunds(&state, 30 * MINUTE_NS + 1, 30 * MINUTE_NS);
        assert_eq!(due, vec![[3; 32]], "the readable one is still refunded");
        assert_eq!(skipped, 2);
    }

    /// The permit window is measured from the quote's expiry, and the boundary second is
    /// inside it.
    #[test]
    fn the_sweep_drops_a_quote_once_its_permit_window_has_closed() {
        quote::clear_pending();
        let q = quote(true, 3_001);
        let deadline = config::get().permit_deadline_s;
        let hash = quote::register(q.clone(), q.expires_at_s - 5).expect("a live quote registers");

        assert_eq!(run_expiry_sweep(q.expires_at_s + deadline).dropped, 0);
        assert!(
            quote::get_pending(&hash).is_some(),
            "the window is still open"
        );

        assert_eq!(run_expiry_sweep(q.expires_at_s + deadline + 1).dropped, 1);
        assert_eq!(quote::get_pending(&hash), None);
    }

    /// Eviction is keyed on `expires_at_s`, never on when the quote was registered: a
    /// re-registration resets the latter, and a long-lived quote is not stale at 120s.
    #[test]
    fn the_sweep_keys_eviction_on_the_expiry_and_not_on_the_registration() {
        quote::clear_pending();
        let q = Quote {
            expires_at_s: 1_800_003_600,
            ..quote(true, 3_002)
        };
        let opened_at = q.expires_at_s - 3_600;
        let hash = quote::register(q.clone(), opened_at).expect("a live quote registers");

        // long past the permit window measured from the registration, and nowhere near it
        // measured from the expiry
        assert_eq!(run_expiry_sweep(opened_at + 3_000).dropped, 0);
        assert!(quote::get_pending(&hash).is_some());
    }

    /// The halt is one-way by design: a later clean audit must not clear it, because the
    /// canister may have been halted for a reason the audit no longer sees.
    #[test]
    fn an_audit_failure_halts_and_a_later_pass_does_not_clear_it() {
        set_halted(false);
        record_audit(true);
        assert!(!is_halted(), "a clean audit halts nothing");

        record_audit(false);
        assert!(is_halted());
        assert!(require_not_halted().is_err());

        record_audit(true);
        assert!(is_halted(), "only a human clears a halt");

        set_halted(false);
        assert!(!is_halted());
        assert!(require_not_halted().is_ok());
    }

    /// An empty log folds to the default state, so the audit passes and halts nothing.
    #[test]
    fn a_clean_replay_audit_leaves_the_canister_running() {
        set_halted(false);
        run_replay_audit();
        assert!(!is_halted());
    }

    /// A halted canister writes no event, but the pending store is pre-money hygiene and
    /// keeps being swept.
    #[test]
    fn a_halted_sweep_still_drops_stale_quotes() {
        quote::clear_pending();
        set_halted(true);
        let q = quote(true, 3_003);
        let hash = quote::register(q.clone(), q.expires_at_s).expect("a live quote registers");

        let swept = run_expiry_sweep(q.expires_at_s + config::get().permit_deadline_s + 1);
        assert_eq!(swept.dropped, 1);
        assert_eq!(swept.refunds, 0);
        assert_eq!(quote::get_pending(&hash), None);
        set_halted(false);
    }
}
