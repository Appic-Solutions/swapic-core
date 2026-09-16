use super::*;
use crate::storage::halt::set_halted;
use settlement_api::types::swap::SwapState;
use types::{ChainId, GasMode, QuoteHash, Rail, TokenAmount};

fn quote(auto_refund: bool, nonce: u64) -> Quote {
    Quote {
        version: 1,
        src_chain: ChainId::BASE,
        src_token: "usdc".parse().unwrap(),
        amount_in: TokenAmount::from(25_000_000_u32),
        dst_chain: ChainId::ARBITRUM,
        dst_token: "usdc".parse().unwrap(),
        expected_out: TokenAmount::from(24_990_000_u32),
        min_out: TokenAmount::from(24_900_000_u32),
        dst_address: "0xuser".parse().unwrap(),
        refund_address: None,
        auto_refund,
        gas_mode: GasMode::Gasless,
        rail: Rail::CctpV2Fast,
        expires_at: UnixSeconds::new(1_800_000_000),
        nonce,
    }
}

fn quote_bytes(q: &Quote) -> Vec<u8> {
    q.canonical_bytes()
}

fn expires_at_s(q: &Quote) -> u64 {
    q.expires_at.get()
}

fn registered_at(q: &Quote, now_s: u64) -> QuoteHash {
    pending_quotes::register(q.clone(), UnixSeconds::new(now_s)).expect("a live quote registers")
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
    pending_quotes::clear_pending();
    let q = quote(true, 3_001);
    let deadline = config::get().permit_deadline_s;
    let hash = registered_at(&q, expires_at_s(&q) - 5);

    assert_eq!(run_expiry_sweep(expires_at_s(&q) + deadline).dropped, 0);
    assert!(
        pending_quotes::get_pending(&hash).is_some(),
        "the window is still open"
    );

    assert_eq!(run_expiry_sweep(expires_at_s(&q) + deadline + 1).dropped, 1);
    assert_eq!(pending_quotes::get_pending(&hash), None);
}

/// Eviction is keyed on `expires_at_s`, never on when the quote was registered: a
/// re-registration resets the latter, and a long-lived quote is not stale at 120s.
#[test]
fn the_sweep_keys_eviction_on_the_expiry_and_not_on_the_registration() {
    pending_quotes::clear_pending();
    let q = Quote {
        expires_at: UnixSeconds::new(1_800_003_600),
        ..quote(true, 3_002)
    };
    let opened_at = expires_at_s(&q) - 3_600;
    let hash = registered_at(&q, opened_at);

    // long past the permit window measured from the registration, and nowhere near it
    // measured from the expiry
    assert_eq!(run_expiry_sweep(opened_at + 3_000).dropped, 0);
    assert!(pending_quotes::get_pending(&hash).is_some());
}

/// A halted canister writes no event, but the pending store is pre-money hygiene and
/// keeps being swept.
#[test]
fn a_halted_sweep_still_drops_stale_quotes() {
    pending_quotes::clear_pending();
    set_halted(true);
    let q = quote(true, 3_003);
    let hash = registered_at(&q, expires_at_s(&q));

    let swept = run_expiry_sweep(expires_at_s(&q) + config::get().permit_deadline_s + 1);
    assert_eq!(swept.dropped, 1);
    assert_eq!(swept.refunds, 0);
    assert_eq!(pending_quotes::get_pending(&hash), None);
    set_halted(false);
}
