use super::*;
use crate::storage::halt::set_halted;
use crate::storage::on_fresh_memory;
use types::events::Choice;
use types::{ChainId, GasMode, Rail, TokenAmount, UnixSeconds, WaitingKey};

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
    q.canonical_bytes().unwrap()
}

fn expires_at_s(q: &Quote) -> u64 {
    q.expires_at.get()
}

fn registered_at(q: &Quote, now_s: u64) -> QuoteHash {
    pending_quotes::register(q.clone(), UnixSeconds::new(now_s)).expect("a live quote registers")
}

/// The canister clock at a whole second.
fn at(secs: u64) -> Timestamp {
    Timestamp::from_secs(secs).unwrap()
}

fn qh(byte: u8) -> QuoteHash {
    QuoteHash::new([byte; 32])
}

/// A swap parked on a user decision since `since_ns`, carrying `bytes` as the quote the
/// sweep will read `auto_refund` out of.
fn waiting(bytes: Vec<u8>, since_ns: u64) -> Swap {
    Swap {
        quote_bytes: bytes,
        status: SwapStatus::WaitingForUser,
        last_attempt: None,
        open_attempt: None,
        src_chain: ChainId::BASE,
        src_token: "usdc".parse().unwrap(),
        amount_in: TokenAmount::from(25_000_000_u32),
        amount_paid: None,
        waiting_since: Some(Timestamp::from_nanos(since_ns)),
    }
}

const MINUTE_NS: u64 = 60 * 1_000_000_000;

const TIMEOUT: Duration = Duration::from_secs(30 * 60);

fn due_at(swaps: &[(QuoteHash, Swap)], now_ns: u64) -> (Vec<QuoteHash>, usize) {
    due_refunds(swaps.to_vec(), Timestamp::from_nanos(now_ns), TIMEOUT)
}

/// The dual refund policy, at the point the timer decides: only a swap whose quote says
/// `auto_refund` is refunded on its own. The others keep waiting for a human.
#[test]
fn due_refunds_picks_the_timed_out_auto_refund_swaps_only() {
    let auto = quote_bytes(&quote(true, 1));
    let manual = quote_bytes(&quote(false, 2));
    let swaps = vec![
        (qh(1), waiting(auto.clone(), 0)),
        (qh(2), waiting(manual, 0)),
        // asked for a refund, but not yet timed out
        (qh(3), waiting(auto.clone(), 25 * MINUTE_NS)),
        // not waiting for anyone
        (
            qh(4),
            Swap {
                status: SwapStatus::Executing,
                waiting_since: None,
                ..waiting(auto, 0)
            },
        ),
    ];
    let (due, skipped) = due_at(&swaps, 30 * MINUTE_NS + 1);
    assert_eq!(due, vec![qh(1)]);
    assert_eq!(skipped, 0);
}

/// "Older than the timeout" is strict: the deadline second itself is still the user's.
#[test]
fn due_refunds_fires_the_moment_after_the_timeout_and_not_before() {
    let swaps = vec![(qh(1), waiting(quote_bytes(&quote(true, 1)), 100))];
    let timeout = 30 * MINUTE_NS;
    assert!(due_at(&swaps, 100 + timeout).0.is_empty());
    assert_eq!(due_at(&swaps, 100 + timeout + 1).0, vec![qh(1)]);
}

/// `quote_bytes` reaches the log from a caller, so the sweep has to survive bytes it
/// cannot read. It counts them and moves on rather than trapping the whole pass.
#[test]
fn due_refunds_is_total_when_quote_bytes_do_not_parse() {
    let swaps = vec![
        (qh(1), waiting(vec![], 0)),
        (qh(2), waiting(vec![0xff; 9], 0)),
        (qh(3), waiting(quote_bytes(&quote(true, 1)), 0)),
    ];
    let (due, skipped) = due_at(&swaps, 30 * MINUTE_NS + 1);
    assert_eq!(due, vec![qh(3)], "the readable one is still refunded");
    assert_eq!(skipped, 2);
}

/// The permit window is measured from the quote's expiry, and the boundary second is
/// inside it.
#[test]
fn the_sweep_drops_a_quote_once_its_permit_window_has_closed() {
    pending_quotes::clear_pending();
    let q = quote(true, 3_001);
    let deadline = config::get().permit_deadline.as_secs();
    let hash = registered_at(&q, expires_at_s(&q) - 5);

    assert_eq!(run_expiry_sweep(at(expires_at_s(&q) + deadline)).dropped, 0);
    assert!(
        pending_quotes::get_pending(&hash).is_some(),
        "the window is still open"
    );

    assert_eq!(
        run_expiry_sweep(at(expires_at_s(&q) + deadline + 1)).dropped,
        1
    );
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
    assert_eq!(run_expiry_sweep(at(opened_at + 3_000)).dropped, 0);
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

    let swept = run_expiry_sweep(at(expires_at_s(&q)
        + config::get().permit_deadline.as_secs()
        + 1));
    assert_eq!(swept.dropped, 1);
    assert_eq!(swept.refunds, 0);
    assert_eq!(pending_quotes::get_pending(&hash), None);
    set_halted(false);
}

fn append_at(payload: EventType, at: Timestamp) {
    events::append_event_at(payload, at).expect("the fold admits it");
}

/// Funds a swap for `q` at `at` and returns its id.
fn funded(q: &Quote, at: Timestamp) -> QuoteHash {
    let quote_hash = q.hash().unwrap();
    append_at(
        EventType::FundsReceived {
            quote_hash,
            quote_bytes: quote_bytes(q),
            chain_id: q.src_chain,
            token: q.src_token.clone(),
            amount: q.amount_in,
            tx_ref: format!("0xdeposit{}", q.nonce),
        },
        at,
    );
    quote_hash
}

fn ask_at(quote_hash: QuoteHash, at: Timestamp) {
    append_at(
        EventType::DecisionRequired {
            quote_hash,
            reason: "slippage".into(),
        },
        at,
    );
}

fn status(quote_hash: &QuoteHash) -> SwapStatus {
    events::read_state(|state| state.swap(quote_hash).unwrap().status)
}

/// The sweep reads the waiting index, so hundreds of closed swaps and swaps that stopped
/// waiting cost it nothing, and it refunds exactly the timed-out waiting swaps whose quote
/// asked for it, once.
#[test]
fn the_sweep_refunds_exactly_the_timed_out_waiting_swaps_among_many_closed_ones() {
    on_fresh_memory(|| {
        let timeout = config::get().decision_timeout;
        let start = at(1_700_000_000);
        let later = |secs| start.checked_add(Duration::from_secs(secs)).unwrap();

        let closed: Vec<QuoteHash> = (0..300)
            .map(|nonce| {
                let quote_hash = funded(&quote(true, nonce), start);
                append_at(
                    EventType::Frozen {
                        quote_hash,
                        reason: "settled".into(),
                    },
                    start,
                );
                quote_hash
            })
            .collect();
        // waited, then stopped: an answer, a refund by hand, a freeze
        let answered = funded(&quote(true, 1_000), start);
        ask_at(answered, start);
        append_at(
            EventType::DecisionMade {
                quote_hash: answered,
                choice: Choice::Requote,
            },
            later(1),
        );
        let refunded = funded(&quote(true, 1_001), start);
        ask_at(refunded, start);
        append_at(
            EventType::RefundStarted {
                quote_hash: refunded,
                reason: "operator".into(),
            },
            later(1),
        );
        let frozen = funded(&quote(true, 1_002), start);
        ask_at(frozen, start);
        append_at(
            EventType::Frozen {
                quote_hash: frozen,
                reason: "sanctions".into(),
            },
            later(1),
        );
        // still waiting: two timed out with auto_refund, one timed out without, one young
        let due = [2_000, 2_001].map(|nonce| funded(&quote(true, nonce), start));
        let manual = funded(&quote(false, 2_002), start);
        let young = funded(&quote(true, 2_003), start);
        for quote_hash in due.into_iter().chain([manual]) {
            ask_at(quote_hash, start);
        }
        ask_at(young, later(600));

        let now = start.checked_add(timeout).unwrap();
        let first_moment = Timestamp::from_nanos(now.as_nanos() + 1);
        assert_eq!(
            run_expiry_sweep(now).refunds,
            0,
            "the deadline itself is still the user's"
        );
        assert_eq!(
            run_expiry_sweep(first_moment),
            Sweep {
                dropped: 0,
                refunds: 2,
                skipped: 0
            }
        );

        for quote_hash in &due {
            assert_eq!(status(quote_hash), SwapStatus::Refunding);
        }
        assert_eq!(status(&manual), SwapStatus::WaitingForUser);
        assert_eq!(status(&young), SwapStatus::WaitingForUser);
        assert_eq!(status(&answered), SwapStatus::Executing);
        assert_eq!(status(&refunded), SwapStatus::Refunding);
        assert!(closed
            .iter()
            .chain([&frozen])
            .all(|q| status(q) == SwapStatus::Frozen));
        let mut still_waiting = vec![
            WaitingKey {
                since: start,
                quote_hash: manual,
            },
            WaitingKey {
                since: later(600),
                quote_hash: young,
            },
        ];
        still_waiting.sort();
        assert_eq!(
            events::read_state(|state| state.store().waiting()),
            still_waiting
        );

        assert_eq!(run_expiry_sweep(first_moment).refunds, 0, "and only once");
        assert!(events::verify_replay());
    });
}
