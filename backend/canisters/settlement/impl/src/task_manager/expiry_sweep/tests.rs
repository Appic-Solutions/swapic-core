use super::*;
use crate::state::MemoryStore;
use crate::storage::halt::set_halted;
use crate::storage::on_fresh_memory;
use types::config::{EvictionsPerSweep, RefundsPerSweep};
use types::events::Choice;
use types::{
    ChainId, GasMode, Quote, Rail, Swap, SwapStatus, TokenAmount, UnixSeconds, WaitingKey,
};

/// Moves both per-tick caps, without the log line `config::set` would write: a unit test has
/// no canister clock to seal one on.
fn set_caps(refunds: u32, evictions: u32) {
    config::test_set(types::Config {
        max_refunds_per_sweep: RefundsPerSweep::new(refunds),
        max_evictions_per_sweep: EvictionsPerSweep::new(evictions),
        ..config::get()
    });
}

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
        // the store takes only a quote a refund can be paid on
        refund_address: Some(
            "0x7551A66653f9a20979ed81835a0b7008EC83401b"
                .parse()
                .unwrap(),
        ),
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
        last_leg: None,
        last_outcome: None,
        last_tx_hash: None,
        paid_out: None,
    }
}

const MINUTE_NS: u64 = 60 * 1_000_000_000;

const TIMEOUT: Duration = Duration::from_secs(30 * 60);

fn key(since_ns: u64, quote_hash: QuoteHash) -> WaitingKey {
    WaitingKey {
        since: Timestamp::from_nanos(since_ns),
        quote_hash,
    }
}

/// A heap store holding `swaps` and exactly the index `keys` given, planted as they are, so
/// the head is read the way the sweep reads it and an entry can say what its swap does not.
fn planted(swaps: Vec<(QuoteHash, Swap)>, keys: Vec<WaitingKey>) -> MemoryStore {
    let mut store = MemoryStore::default();
    for (quote_hash, swap) in swaps {
        store.put_swap(quote_hash, swap);
    }
    for key in keys {
        store.put_auto_refund_waiting(key);
    }
    store
}

fn head_at(store: &MemoryStore, now_ns: u64, cap: usize) -> Head {
    timed_out_waiting(store, Timestamp::from_nanos(now_ns), TIMEOUT, cap)
}

/// The dual refund policy, at the point the timer decides: a timed-out entry is due only
/// when it is the wait its swap is in, and that swap asks for an automatic refund. Every
/// other entry at the head is stale, and an entry not yet timed out is not at the head.
#[test]
fn the_head_is_the_due_auto_refund_waits_and_everything_else_is_stale() {
    let auto = quote_bytes(&quote(true, 1));
    let manual = quote_bytes(&quote(false, 2));
    let store = planted(
        vec![
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
                    ..waiting(auto.clone(), 0)
                },
            ),
            // waiting since an instant its entry does not say
            (qh(6), waiting(auto, 1)),
        ],
        vec![
            key(0, qh(1)),
            key(0, qh(2)),
            key(25 * MINUTE_NS, qh(3)),
            key(0, qh(4)),
            // no such swap
            key(0, qh(5)),
            key(0, qh(6)),
        ],
    );
    let head = head_at(&store, 30 * MINUTE_NS + 1, 100);
    assert_eq!(head.due, vec![qh(1)]);
    assert_eq!(
        head.stale,
        vec![key(0, qh(2)), key(0, qh(4)), key(0, qh(5)), key(0, qh(6))]
    );
    assert!(!head.more);
}

/// "Older than the timeout" is strict: the deadline instant itself is still the user's. And
/// the head is bounded by the cap, saying when more is due behind it.
#[test]
fn the_head_fires_the_moment_after_the_timeout_and_stops_at_the_cap() {
    let auto = quote_bytes(&quote(true, 1));
    let store = planted(
        vec![
            (qh(1), waiting(auto.clone(), 100)),
            (qh(2), waiting(auto.clone(), 101)),
            (qh(3), waiting(auto, 102)),
        ],
        vec![key(100, qh(1)), key(101, qh(2)), key(102, qh(3))],
    );
    let timeout = 30 * MINUTE_NS;
    assert!(head_at(&store, 100 + timeout, 100).due.is_empty());
    assert_eq!(head_at(&store, 100 + timeout + 1, 100).due, vec![qh(1)]);
    let all = head_at(&store, 102 + timeout + 1, 100);
    assert_eq!((all.due, all.more), (vec![qh(1), qh(2), qh(3)], false));
    let capped = head_at(&store, 102 + timeout + 1, 2);
    assert_eq!((capped.due, capped.more), (vec![qh(1), qh(2)], true));
}

/// `quote_bytes` reaches the log from a caller, so the rule has to survive bytes it cannot
/// read: an entry whose swap has no quote behind it says a wait the timer cannot act on,
/// so it is stale and repaired, and the pass goes on to the ones that are due.
#[test]
fn an_entry_whose_bytes_are_no_quote_is_stale() {
    let store = planted(
        vec![
            (qh(1), waiting(vec![], 0)),
            (qh(2), waiting(vec![0xff; 9], 0)),
            (qh(3), waiting(quote_bytes(&quote(true, 1)), 0)),
        ],
        vec![key(0, qh(1)), key(0, qh(2)), key(0, qh(3))],
    );
    let head = head_at(&store, 30 * MINUTE_NS + 1, 100);
    assert_eq!(head.due, vec![qh(3)], "the readable one is still refunded");
    assert_eq!(head.stale, vec![key(0, qh(1)), key(0, qh(2))]);
}

/// The permit window is measured from the quote's expiry, and the boundary second is
/// inside it.
#[test]
fn the_sweep_drops_a_quote_once_its_permit_window_has_closed() {
    pending_quotes::clear();
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

/// Eviction is keyed on `expires_at_s`, never on when the quote was registered: the stable
/// store keeps a bare `Quote` with no registration time in it, and a long-lived quote is
/// not stale 120s after it was registered.
#[test]
fn the_sweep_keys_eviction_on_the_expiry_and_not_on_the_registration() {
    pending_quotes::clear();
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
    pending_quotes::clear();
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
                skipped: 0,
                stale: 0,
                more: false
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
        assert_eq!(
            events::read_state(|state| state.store().auto_refund_waiting()),
            vec![WaitingKey {
                since: later(600),
                quote_hash: young,
            }],
            "the index holds the waits a timer can act on, so the manual one is not in it"
        );
        assert_eq!(
            events::read_state(|state| state.swap(&manual).unwrap().waiting_since),
            Some(start),
            "and the manual swap still carries its wait, for whoever asks it"
        );

        assert_eq!(run_expiry_sweep(first_moment).refunds, 0, "and only once");
        assert!(events::verify_replay());
    });
}

/// What the cap put at risk, and why only auto-refundable swaps are indexed: a swap that
/// waits for a human never leaves the index on its own, so a cap's worth of them at the head
/// of it would starve every automatic refund behind them for good. The earlier test could not
/// see this, because every due swap in its fixture asked for an automatic refund.
#[test]
fn consult_me_waiters_older_than_every_auto_refund_one_do_not_hold_up_the_cap() {
    on_fresh_memory(|| {
        let cap = 3;
        set_caps(cap as u32, 200);
        let timeout = config::get().decision_timeout;
        let start = at(1_700_000_000);
        let asked_at = |n: u64| Timestamp::from_nanos(start.as_nanos() + n);

        // a cap's worth of consult-me swaps, every one of them older than every auto one
        let manual: Vec<QuoteHash> = (0..cap as u64)
            .map(|nonce| {
                let quote_hash = funded(&quote(false, 100 + nonce), start);
                ask_at(quote_hash, asked_at(nonce));
                quote_hash
            })
            .collect();
        let auto: Vec<QuoteHash> = (0..cap as u64)
            .map(|nonce| {
                let quote_hash = funded(&quote(true, 200 + nonce), start);
                ask_at(quote_hash, asked_at(100 + nonce));
                quote_hash
            })
            .collect();

        let now = Timestamp::from_nanos(
            asked_at(200)
                .checked_add(timeout)
                .expect("a timeout inside the clock")
                .as_nanos(),
        );
        let swept = run_expiry_sweep(now);
        assert_eq!(
            (swept.refunds, swept.stale, swept.more),
            (cap, 0, false),
            "one tick reaches every automatic refund"
        );
        assert!(auto.iter().all(|q| status(q) == SwapStatus::Refunding));
        assert!(manual
            .iter()
            .all(|q| status(q) == SwapStatus::WaitingForUser));
        assert!(events::verify_replay());
    });
}

/// The other half of the dual policy, through the timer: a swap whose quote asks for a human
/// is never refunded on its own, however long it waits.
#[test]
fn a_swap_that_waits_for_a_human_is_never_refunded_by_the_timer() {
    on_fresh_memory(|| {
        let start = at(1_700_000_000);
        let manual = funded(&quote(false, 7), start);
        ask_at(manual, start);
        assert!(
            events::read_state(|state| state.store().auto_refund_waiting()).is_empty(),
            "it is in no index"
        );

        for now in [
            start.checked_add(config::get().decision_timeout).unwrap(),
            at(1_800_000_000),
            Timestamp::from_nanos(u64::MAX),
        ] {
            let swept = run_expiry_sweep(now);
            assert_eq!((swept.refunds, swept.stale, swept.more), (0, 0, false));
        }
        assert_eq!(status(&manual), SwapStatus::WaitingForUser);
        assert!(events::verify_replay());
    });
}

/// A pass that refunded every due swap at once would trap past the instruction budget, and
/// the repeating timer would retry the same batch forever, taking the eviction pass down
/// with it. So one pass takes at most its cap, oldest first, and the passes after it drain
/// the rest, while the eviction pass runs in the same tick whatever the refund pass does.
#[test]
fn the_sweep_refunds_at_most_the_cap_per_tick_and_drains_the_rest_on_the_next_ticks() {
    on_fresh_memory(|| {
        let cap = 3;
        set_caps(cap as u32, 200);
        let timeout = config::get().decision_timeout;
        let start = at(1_700_000_000);

        // three times the cap, each waiting since a distinct instant, so "oldest first" is
        // an order and not a coincidence
        let due: Vec<QuoteHash> = (0..3 * cap as u64)
            .map(|nonce| {
                let quote_hash = funded(&quote(true, nonce), start);
                ask_at(quote_hash, Timestamp::from_nanos(start.as_nanos() + nonce));
                quote_hash
            })
            .collect();
        // a stale pending quote, to prove the eviction pass ran in the same tick
        let stale = Quote {
            expires_at: UnixSeconds::new(start.as_secs().get()),
            ..quote(true, 9_000)
        };
        let pending = registered_at(&stale, expires_at_s(&stale) - 10);

        let now = Timestamp::from_nanos(
            start
                .checked_add(timeout)
                .expect("a timeout inside the clock")
                .as_nanos()
                + 3 * cap as u64,
        );
        let first = run_expiry_sweep(now);
        assert_eq!((first.refunds, first.more), (cap, true));
        assert_eq!(
            due.iter()
                .filter(|q| status(q) == SwapStatus::Refunding)
                .count(),
            cap,
            "the cap, and no more"
        );
        for quote_hash in due.iter().take(cap) {
            assert_eq!(
                status(quote_hash),
                SwapStatus::Refunding,
                "the oldest waits go first"
            );
        }
        assert_eq!(pending_quotes::get_pending(&pending), None);
        assert_eq!(first.dropped, 1, "the eviction pass ran in the same tick");

        let second = run_expiry_sweep(now);
        assert_eq!((second.refunds, second.more), (cap, true));
        let third = run_expiry_sweep(now);
        assert_eq!(
            (third.refunds, third.more),
            (cap, false),
            "the third tick drains the rest and reports nothing left"
        );
        assert!(due.iter().all(|q| status(q) == SwapStatus::Refunding));
        assert_eq!(run_expiry_sweep(now).refunds, 0);
        assert!(events::verify_replay());
    });
}
