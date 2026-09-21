use super::*;
use crate::state::transitions::tests::{funds, funds_manual, manual_swap_id, swap_id};
use crate::state::transitions::ReplayError;
use crate::state::MemoryStore;
use crate::storage::audit_cursor;
use crate::storage::halt::is_halted;
use crate::storage::on_fresh_memory;
use crate::task_manager::expiry_sweep::run_expiry_sweep;
use crate::task_manager::replay_audit::{run_audit_replay_step, run_replay_audit};
use types::config::{AuditChunk, RefundsPerSweep};
use types::events::Choice;
use types::quote::QuoteError;
use types::LedgerMeta;
use types::{
    Attempt, GasMode, Quote, Rail, SwapStatus, TokenAmount, TxHash, UnixSeconds, WaitingKey,
};

fn fold_both(payloads: Vec<EventType>) -> (State<StableStore>, State<MemoryStore>) {
    let mut stable = State::new(StableStore(()));
    let mut heap = State::<MemoryStore>::default();
    for (i, payload) in (0..).zip(payloads) {
        stable.check(&payload).expect("guard admits");
        let meta = stable.meta();
        let event = Event::seal(
            meta.next_event_index,
            Timestamp::from_nanos(i),
            meta.last_event_hash,
            payload,
        )
        .unwrap();
        apply_state_transition(&mut stable, &event);
        apply_state_transition(&mut heap, &event);
    }
    (stable, heap)
}

/// One copy of the transition rules serves both stores, so the stable fold and the heap
/// fold of the same events are the same fold: what the replay audit relies on.
#[test]
fn the_stable_fold_matches_the_heap_fold() {
    let quote = swap_id(7);
    let amount = |value: u32| TokenAmount::from(value);
    let (stable, heap) = fold_both(vec![
        EventType::PocketFunded {
            chain_id: ChainId::BASE,
            amount: amount(1_000),
        },
        funds(7),
        EventType::PocketReserved {
            quote_hash: quote,
            chain_id: ChainId::BASE,
            amount: amount(400),
        },
        EventType::TxSigned {
            quote_hash: quote,
            attempt: Attempt::FIRST,
            chain_id: ChainId::BASE,
            tx_hash: TxHash::new([1; 32]),
            raw_tx: vec![],
        },
    ]);
    assert!(heap.matches(&stable));
    assert_eq!(stable.meta().next_event_index, EventIndex::new(4));
    assert_eq!(stable.pocket(&ChainId::BASE).unwrap().reserved, amount(400));
    assert_eq!(
        stable.swap(&quote).unwrap().open_attempt,
        Some(Attempt::FIRST)
    );
}

fn append(payload: EventType) {
    append_event_at(payload, Timestamp::from_nanos(1)).expect("the stable fold admits it");
}

fn log_replay() -> Result<State<MemoryStore>, ReplayError> {
    EVENTS.with(|e| replay(e.borrow().iter()))
}

/// The deep check an operator runs by hand, in one step the size of the log: the comparison
/// a bounded timer pass cannot make. Answers whether it halted the canister.
fn deep_check_halts() -> bool {
    let step = run_audit_replay_step(event_count());
    assert!(
        step.finished || step.halted,
        "one step the size of the log ends the audit: {step:?}"
    );
    assert_eq!(step.halted, is_halted(), "a halting step sets the flag");
    step.halted
}

/// A refused append writes nothing: no log entry, no swap, no moved fold. The pair the
/// guard binds is the one the expiry sweep reads its refund policy out of.
#[test]
fn an_append_of_a_mismatched_quote_pair_writes_nothing() {
    on_fresh_memory(|| {
        let EventType::FundsReceived {
            quote_bytes,
            chain_id,
            token,
            amount,
            tx_ref,
            ..
        } = funds(1)
        else {
            panic!("the fixture is a FundsReceived");
        };
        let swapped = EventType::FundsReceived {
            quote_hash: swap_id(2),
            quote_bytes,
            chain_id,
            token,
            amount,
            tx_ref,
        };
        assert_eq!(
            append_event_at(swapped, Timestamp::from_nanos(1)),
            Err(AppendError::Transition(
                TransitionError::QuoteHashMismatch {
                    declared: swap_id(2),
                    computed: swap_id(1)
                }
            ))
        );
        assert_eq!(event_count(), 0);
        assert_eq!(read_state(|state| state.store().swap(&swap_id(2))), None);
        assert_eq!(ensure_fold_in_step(), Ok(()));

        append(funds(1));
        assert_eq!(event_count(), 1, "and the matching pair still lands");
    });
}

/// The preimage the log records is a quote this canister would hold, not merely one it can
/// parse: `record_decision_required` reads the refund policy out of exactly these bytes, and
/// `register_quote` refuses the same quotes at the other door.
#[test]
fn an_append_of_a_preimage_that_does_not_validate_writes_nothing() {
    on_fresh_memory(|| {
        let funds_for = |q: &Quote| {
            let quote_bytes = q.canonical_bytes().expect("the fixture has a preimage");
            EventType::FundsReceived {
                quote_hash: types::quote::quote_hash_of(&quote_bytes),
                quote_bytes,
                chain_id: q.src_chain,
                token: q.src_token.clone(),
                amount: q.amount_in,
                tx_ref: "0xdeposit".into(),
            }
        };
        let refused = [
            (
                Quote {
                    version: 2,
                    ..crate::state::transitions::tests::quote(1)
                },
                QuoteError::UnsupportedVersion(2),
            ),
            (
                Quote {
                    src_token: "".parse().unwrap(),
                    ..crate::state::transitions::tests::quote(2)
                },
                QuoteError::EmptyText { field: "src_token" },
            ),
        ];
        for (invalid, error) in refused {
            let payload = funds_for(&invalid);
            assert_eq!(
                append_event_at(payload, Timestamp::from_nanos(1)),
                Err(AppendError::Transition(TransitionError::UnparseableQuote(
                    error
                )))
            );
            assert_eq!(event_count(), 0, "and nothing was written");
            assert_eq!(ensure_fold_in_step(), Ok(()));
        }

        append(funds(1));
        assert_eq!(event_count(), 1, "a quote that validates still lands");
    });
}

/// Moves the audit chunk, without the log line `config::set` would write: a unit test has no
/// canister clock to seal one on.
fn set_chunk(events: u32) {
    crate::storage::config::test_set(types::Config {
        audit_chunk_events: AuditChunk::new(events),
        ..crate::storage::config::get()
    });
}

/// A broken link inside the log, with the fold in step with it: the head check is O(1) and
/// cannot see it, and the chain audit only reaches it once its rolling cursor gets there. So
/// the pass that covers the entry is the pass that halts, and the cursor stops on it.
#[test]
fn the_chain_audit_halts_in_the_pass_that_covers_a_tampered_entry() {
    on_fresh_memory(|| {
        set_chunk(2);
        for nonce in 1..=4 {
            append(funds(nonce));
        }
        let meta = StableStore(()).meta();
        // sealed on a parent the log does not end with: no guard would admit it, and no
        // append could write it
        let tampered = Event::seal(
            meta.next_event_index,
            Timestamp::from_nanos(5),
            EventHash::new([7; 32]),
            funds(5),
        )
        .expect("the payload has a preimage");
        test_push_raw(tampered);
        assert_eq!(
            ensure_fold_in_step(),
            Ok(()),
            "the O(1) check is what cannot see this"
        );

        run_replay_audit();
        assert!(!is_halted(), "the first chunk is sound");
        assert_eq!(audit_cursor::get().next_index, EventIndex::new(2));
        run_replay_audit();
        assert!(!is_halted(), "and so is the second");
        assert_eq!(audit_cursor::get().next_index, EventIndex::new(4));

        run_replay_audit();
        assert!(is_halted(), "the pass that reaches the entry halts");
        assert_eq!(
            audit_cursor::get().next_index,
            EventIndex::new(4),
            "and the cursor stays on the entry that failed"
        );
    });
}

/// The cheap invariant runs every tick: a fold whose head left the log's is exactly what
/// `append_event` refuses to write on, so the audit halts on it without folding anything.
#[test]
fn a_fold_out_of_step_with_its_log_halts_the_next_pass() {
    on_fresh_memory(|| {
        append(funds(1));
        run_replay_audit();
        assert!(!is_halted());

        let meta = StableStore(()).meta();
        StableStore(()).put_meta(LedgerMeta {
            last_event_hash: EventHash::new([7; 32]),
            ..meta
        });
        run_replay_audit();
        assert!(is_halted(), "the head check halts the tick");
    });
}

/// A swap sits in the stable fold that no event created, and the log then builds on it.
/// The heap replay refuses the first event on it, so the audit answers false and halts,
/// where it used to trap on the fold's `expect` before it could halt.
#[test]
fn replay_audit_halts_on_a_swap_no_event_created() {
    on_fresh_memory(|| {
        let quote = QuoteHash::new([9; 32]);
        let ghost = Swap {
            quote_bytes: vec![],
            status: SwapStatus::FundsReceived,
            last_attempt: None,
            open_attempt: None,
            src_chain: ChainId::BASE,
            src_token: "USDC".parse().unwrap(),
            amount_in: TokenAmount::from(1_u32),
            amount_paid: None,
            waiting_since: None,
        };
        StableStore(()).put_swap(quote, ghost);
        append(EventType::TxSigned {
            quote_hash: quote,
            attempt: Attempt::FIRST,
            chain_id: ChainId::BASE,
            tx_hash: TxHash::new([1; 32]),
            raw_tx: vec![],
        });

        assert!(verify_chain(), "the chain itself is sound");
        assert_eq!(
            log_replay().unwrap_err(),
            ReplayError::Refused {
                index: EventIndex::ZERO,
                error: TransitionError::UnknownSwap(quote)
            }
        );
        assert!(!verify_replay());
        run_replay_audit();
        assert!(
            !is_halted(),
            "a bounded pass checks the chain and the head, not the whole fold"
        );
        assert!(
            deep_check_halts(),
            "the deep check halts instead of trapping"
        );
    });
}

/// A pocket balance sits in the stable fold that no event produced, and the log spends it.
#[test]
fn replay_audit_halts_on_a_pocket_balance_no_event_produced() {
    on_fresh_memory(|| {
        let quote = swap_id(8);
        StableStore(()).put_pocket(
            ChainId::BASE,
            Pocket {
                available: TokenAmount::ZERO,
                reserved: TokenAmount::from(10_u32),
            },
        );
        append(funds(8));
        append(EventType::PocketSpent {
            quote_hash: quote,
            chain_id: ChainId::BASE,
            amount: TokenAmount::from(10_u32),
        });

        assert_eq!(
            log_replay().unwrap_err(),
            ReplayError::Refused {
                index: EventIndex::new(1),
                error: TransitionError::UnknownPocket(ChainId::BASE)
            }
        );
        assert!(!verify_replay());
        run_replay_audit();
        assert!(
            !is_halted(),
            "a bounded pass checks the chain and the head, not the whole fold"
        );
        assert!(deep_check_halts());
    });
}

/// A divergence the log folds cleanly over is still a divergence: the comparison catches
/// what the replay admits.
#[test]
fn replay_audit_halts_on_a_fold_that_replays_cleanly_but_differs() {
    on_fresh_memory(|| {
        let quote = swap_id(6);
        append(funds(6));
        let mut swap = StableStore(()).swap(&quote).unwrap();
        swap.status = SwapStatus::Frozen;
        StableStore(()).put_swap(quote, swap);

        assert!(log_replay().is_ok(), "every event is admissible");
        assert!(!verify_replay());
        run_replay_audit();
        assert!(
            !is_halted(),
            "a bounded pass checks the chain and the head, not the whole fold"
        );
        assert!(deep_check_halts());
    });
}

/// The auto-refund waiting index as a scan of every swap would build it: waiting, and asking
/// for an automatic refund.
fn scanned_waiting(store: &impl Store) -> Vec<WaitingKey> {
    let mut keys: Vec<WaitingKey> = store
        .swaps()
        .into_iter()
        .filter(|(_, swap)| Quote::parse(&swap.quote_bytes).is_ok_and(|quote| quote.auto_refund))
        .filter_map(|(quote_hash, swap)| {
            swap.waiting_since
                .map(|since| WaitingKey { since, quote_hash })
        })
        .collect();
    keys.sort();
    keys
}

fn waiting_key(nanos: u64, quote_hash: QuoteHash) -> WaitingKey {
    WaitingKey {
        since: Timestamp::from_nanos(nanos),
        quote_hash,
    }
}

/// Every transition that starts or stops a waiting clock keeps the index equal to a full
/// scan, in the stable store as each event lands and in the heap fold the audit rebuilds.
#[test]
fn the_waiting_index_matches_a_full_scan_across_every_waiting_transition() {
    on_fresh_memory(|| {
        let [a, b, c, d, e] = [1, 2, 3, 4, 5].map(swap_id);
        // waits like the others and belongs in no index: its quote asks for a human
        let manual = manual_swap_id(6);
        let ask = |quote_hash| EventType::DecisionRequired {
            quote_hash,
            reason: "slippage".into(),
        };
        let decide = |quote_hash, choice| EventType::DecisionMade { quote_hash, choice };
        let refund = |quote_hash| EventType::RefundStarted {
            quote_hash,
            reason: "operator".into(),
        };
        let freeze = |quote_hash| EventType::Frozen {
            quote_hash,
            reason: "sanctions".into(),
        };
        let steps = vec![
            (1, funds(1)),
            (2, funds(2)),
            (3, funds(3)),
            (4, funds(4)),
            (5, funds(5)),
            (6, funds_manual(6)),
            (10, ask(a)),
            // two waits that begin at the same instant are two keys
            (11, ask(b)),
            (11, ask(c)),
            // an answer stops the clock, and a later question starts a new one
            (20, decide(a, Choice::Requote)),
            (21, ask(a)),
            // a refund and a freeze stop it too
            (30, refund(b)),
            (31, freeze(c)),
            // neither touches a swap that was not waiting
            (32, refund(d)),
            (33, freeze(d)),
            (40, decide(a, Choice::Refund)),
            // a refund is one way, so the last wait begins on a swap that is not refunding
            (50, ask(e)),
            // and a wait no timer can act on enters no index
            (51, ask(manual)),
        ];
        for (nanos, payload) in steps {
            append_event_at(payload.clone(), Timestamp::from_nanos(nanos))
                .expect("the fold admits it");
            let (index, scan) = read_state(|state| {
                (
                    state.store().auto_refund_waiting(),
                    scanned_waiting(state.store()),
                )
            });
            assert_eq!(index, scan, "after {payload:?} at {nanos}");
            if nanos == 11 && payload == ask(c) {
                assert_eq!(
                    index,
                    vec![waiting_key(10, a), waiting_key(11, b), waiting_key(11, c)]
                );
            }
        }
        assert_eq!(
            read_state(|state| state.store().auto_refund_waiting()),
            vec![waiting_key(50, e)],
            "the consult-me wait is not in the index"
        );
        assert_eq!(
            read_state(|state| state.swap(&manual).unwrap().waiting_since),
            Some(Timestamp::from_nanos(51)),
            "and it is on the swap, where a caller reads it"
        );

        let heap = log_replay().expect("the log folds");
        assert_eq!(
            heap.store().auto_refund_waiting(),
            scanned_waiting(heap.store())
        );
        assert!(verify_replay(), "and the audit compares the two indexes");
    });
}

/// The index is part of the fold, so an entry no event made is a divergence the audit
/// halts on.
#[test]
fn replay_audit_halts_on_a_waiting_entry_no_event_made() {
    on_fresh_memory(|| {
        let quote = swap_id(5);
        append(funds(5));
        assert!(verify_replay());
        StableStore(()).put_auto_refund_waiting(waiting_key(7, quote));

        assert!(!verify_replay());
        run_replay_audit();
        assert!(
            !is_halted(),
            "a bounded pass checks the chain and the head, not the whole fold"
        );
        assert!(deep_check_halts());
    });
}

/// The index is the queue, so an entry whose swap is not waiting any more is dropped by the
/// pass that meets it: left in place it would hold a slot of every pass's cap for good. Only
/// a fold no event could have produced has such an entry, and the drop is a logged event, so
/// the repair is in the record the deep check reads rather than a silent edit of the fold.
#[test]
fn the_sweep_repairs_an_index_entry_whose_swap_stopped_waiting_through_the_log() {
    on_fresh_memory(|| {
        append(funds(1));
        let orphan = QuoteHash::new([9; 32]);
        // a swap that never waited, and an entry naming no swap at all
        StableStore(()).put_auto_refund_waiting(waiting_key(1, swap_id(1)));
        StableStore(()).put_auto_refund_waiting(waiting_key(2, orphan));
        assert!(!verify_replay(), "neither entry is a fold of the log");
        let before = event_count();

        let swept = run_expiry_sweep(Timestamp::from_nanos(u64::MAX));
        assert_eq!((swept.stale, swept.refunds, swept.skipped), (2, 0, 0));
        assert!(read_state(|state| state.store().auto_refund_waiting()).is_empty());
        assert_eq!(
            events_page(before, 2)
                .into_iter()
                .map(|event| event.payload)
                .collect::<Vec<_>>(),
            vec![
                EventType::WaitingRepaired {
                    quote_hash: swap_id(1)
                },
                EventType::WaitingRepaired { quote_hash: orphan },
            ],
            "the pass wrote the repair down"
        );
        assert!(
            verify_chain(),
            "the repairs are links of the chain like every other event"
        );
        assert!(
            verify_replay(),
            "and the fold is the fold of the log again, because the log explains the drop"
        );
        assert!(
            !deep_check_halts(),
            "so the deep audit has nothing to halt on"
        );

        let again = run_expiry_sweep(Timestamp::from_nanos(u64::MAX));
        assert_eq!(
            (again.stale, again.skipped),
            (0, 0),
            "and there is nothing left to repair"
        );
    });
}

/// The repair makes a swap's entries agree with the swap, so turned on a swap that really
/// is waiting it changes nothing: the entry is the index doing its job, and it stays.
#[test]
fn a_repair_of_a_swap_that_really_waits_keeps_its_entry() {
    on_fresh_memory(|| {
        let quote_hash = swap_id(2);
        append(funds(2));
        append(EventType::DecisionRequired {
            quote_hash,
            reason: "slippage".into(),
        });
        let indexed = read_state(|state| state.store().auto_refund_waiting());
        assert_eq!(indexed.len(), 1, "the wait is indexed");

        append_event_at(
            EventType::WaitingRepaired { quote_hash },
            Timestamp::from_nanos(2),
        )
        .expect("a repair is admitted on any swap");
        assert_eq!(
            read_state(|state| state.store().auto_refund_waiting()),
            indexed,
            "the entry is still there"
        );
        assert!(verify_replay());
        assert!(!deep_check_halts());
    });
}

/// What the cap put at risk: an entry the sweep can neither refund nor repair holds a slot
/// of every pass's cap for good, and a cap's worth of them at the head of the index starves
/// every automatic refund behind them. So every entry that is not the wait its swap is in
/// is repaired through the log, whichever way it differs, and the refunds behind them start
/// on the pass after.
#[test]
fn the_sweep_repairs_a_cap_of_unrefundable_entries_and_the_refunds_behind_them_start() {
    on_fresh_memory(|| {
        let cap = 4;
        crate::storage::config::test_set(types::Config {
            max_refunds_per_sweep: RefundsPerSweep::new(cap as u32),
            ..crate::storage::config::get()
        });
        let timeout = crate::storage::config::get().decision_timeout;
        let start = Timestamp::from_secs(1_700_000_000).unwrap();
        let at = |n: u64| Timestamp::from_nanos(start.as_nanos() + n);
        let ask = |quote_hash, nanos| {
            append_event_at(
                EventType::DecisionRequired {
                    quote_hash,
                    reason: "slippage".into(),
                },
                at(nanos),
            )
            .expect("the fold admits it");
        };

        // the swaps the entries will name: one that never waited, one that waits for a
        // human, and one that really waits and asks for a refund
        let [never_waited, real] = [swap_id(1), swap_id(3)];
        let manual = manual_swap_id(2);
        let orphan = QuoteHash::new([9; 32]);
        append(funds(1));
        append(funds_manual(2));
        append(funds(3));
        ask(manual, 5);
        ask(real, 6);
        // a cap's worth of entries no event produced, every one older than every real wait:
        // no swap at all, a swap that never waited, the consult-me swap, and the real wait
        // under an instant it never had
        let mut store = StableStore(());
        let key = |nanos, quote_hash| WaitingKey {
            since: at(nanos),
            quote_hash,
        };
        for planted in [
            key(1, orphan),
            key(2, never_waited),
            key(3, manual),
            key(4, real),
        ] {
            store.put_auto_refund_waiting(planted);
        }
        // and the automatic refunds behind them
        let due = [swap_id(4), swap_id(5)];
        append(funds(4));
        append(funds(5));
        ask(due[0], 7);
        ask(due[1], 8);
        assert!(!verify_replay(), "none of the four is a fold of the log");
        let before = event_count();

        let now = at(8).checked_add(timeout).unwrap();
        let now = Timestamp::from_nanos(now.as_nanos() + 1);
        let first = run_expiry_sweep(now);
        assert_eq!(
            (first.stale, first.refunds, first.skipped, first.more),
            (cap, 0, 0, true),
            "the pass spent its cap on repairs and reports the work behind them"
        );
        assert_eq!(
            events_page(before, cap as u64)
                .into_iter()
                .map(|event| event.payload)
                .collect::<Vec<_>>(),
            [orphan, never_waited, manual, real]
                .map(|quote_hash| EventType::WaitingRepaired { quote_hash })
                .to_vec(),
            "every repair is on the log"
        );
        assert_eq!(
            read_state(|state| state.store().auto_refund_waiting()),
            vec![key(6, real), key(7, due[0]), key(8, due[1])],
            "the real wait is back under its own instant, and the others are gone"
        );
        assert!(
            verify_replay(),
            "the log explains the repairs, so the fold is its fold again"
        );

        let second = run_expiry_sweep(now);
        assert_eq!(
            (second.stale, second.refunds, second.skipped, second.more),
            (0, 3, 0, false),
            "the refunds behind them start on the next pass"
        );
        for quote_hash in [real, due[0], due[1]] {
            assert_eq!(
                read_state(|state| state.swap(&quote_hash).unwrap().status),
                SwapStatus::Refunding
            );
        }
        assert_eq!(
            read_state(|state| state.swap(&manual).unwrap().status),
            SwapStatus::WaitingForUser,
            "the consult-me swap keeps waiting for its human"
        );
        assert!(verify_replay());
        assert!(!deep_check_halts(), "and the audit stays clean");
    });
}

/// The sweep finds timed-out swaps through the index alone: a waiting swap planted in the
/// swaps map with no index entry is invisible to it, which is what keeps a pass from
/// reading every swap ever recorded.
#[test]
fn the_expiry_sweep_reads_the_index_and_not_the_swaps() {
    on_fresh_memory(|| {
        let quote_hash = swap_id(4);
        append(funds(4));
        let auto_refund = Quote {
            version: 1,
            src_chain: ChainId::BASE,
            src_token: "usdc".parse().unwrap(),
            amount_in: TokenAmount::from(1_u32),
            dst_chain: ChainId::ARBITRUM,
            dst_token: "usdc".parse().unwrap(),
            expected_out: TokenAmount::from(1_u32),
            min_out: TokenAmount::from(1_u32),
            dst_address: "0xuser".parse().unwrap(),
            refund_address: None,
            auto_refund: true,
            gas_mode: GasMode::Gasless,
            rail: Rail::CctpV2Fast,
            expires_at: UnixSeconds::new(1),
            nonce: 1,
        };
        let mut swap = StableStore(()).swap(&quote_hash).unwrap();
        swap.quote_bytes = auto_refund.canonical_bytes().unwrap();
        swap.status = SwapStatus::WaitingForUser;
        swap.waiting_since = Some(Timestamp::from_nanos(0));
        StableStore(()).put_swap(quote_hash, swap);

        // a scan would find it timed out, readable and asking for a refund
        let long_after = Timestamp::from_nanos(u64::MAX);
        let swept = run_expiry_sweep(long_after);
        assert_eq!((swept.refunds, swept.skipped), (0, 0));
        assert_eq!(
            read_state(|state| state.swap(&quote_hash).unwrap().status),
            SwapStatus::WaitingForUser
        );
    });
}

/// The O(1) rule `post_upgrade` refuses an upgrade on and `append_event` refuses to write
/// on: the fold seals next at the log's length, linked to the log's last hash.
#[test]
fn the_fold_check_passes_a_fold_in_step_and_names_what_is_not() {
    on_fresh_memory(|| {
        assert_eq!(
            ensure_fold_in_step(),
            Ok(()),
            "genesis: no events, zero head"
        );
        let zero = StableStore(()).meta();
        StableStore(()).put_meta(LedgerMeta {
            last_event_hash: EventHash::new([7; 32]),
            ..zero
        });
        assert_eq!(
            ensure_fold_in_step(),
            Err(FoldOutOfStep::Head {
                log: EventHash::ZERO,
                fold: EventHash::new([7; 32])
            }),
            "an empty log ends with the zero hash"
        );
        StableStore(()).put_meta(zero);

        append(funds(1));
        append(funds(2));
        assert_eq!(ensure_fold_in_step(), Ok(()));
        let meta = StableStore(()).meta();

        StableStore(()).put_meta(LedgerMeta {
            next_event_index: EventIndex::new(3),
            ..meta
        });
        assert_eq!(
            ensure_fold_in_step(),
            Err(FoldOutOfStep::Length {
                log_len: 2,
                next_event_index: EventIndex::new(3)
            })
        );
        assert_eq!(
            append_event_at(funds(3), Timestamp::from_nanos(1)),
            Err(AppendError::IndexMismatch {
                log_len: 2,
                sealed: EventIndex::new(3)
            })
        );

        StableStore(()).put_meta(LedgerMeta {
            last_event_hash: EventHash::new([7; 32]),
            ..meta
        });
        assert_eq!(
            ensure_fold_in_step(),
            Err(FoldOutOfStep::Head {
                log: meta.last_event_hash,
                fold: EventHash::new([7; 32])
            })
        );
        assert!(matches!(
            append_event_at(funds(3), Timestamp::from_nanos(1)),
            Err(AppendError::ChainDiverged { .. })
        ));
        assert_eq!(event_count(), 2, "a refused append writes nothing");
    });
}

/// One repair makes every entry of a swap right, so a swap with several stale entries at
/// the head of the index is repaired by one line and not one per entry: the others would be
/// no-op lines on a permanent log, each holding a slot of the pass's cap.
#[test]
fn several_stale_entries_for_one_swap_are_repaired_by_one_line() {
    on_fresh_memory(|| {
        append(funds(1));
        let quote_hash = swap_id(1);
        for nanos in [1, 2, 3] {
            StableStore(()).put_auto_refund_waiting(waiting_key(nanos, quote_hash));
        }
        let before = event_count();

        let swept = run_expiry_sweep(Timestamp::from_nanos(u64::MAX));
        assert_eq!((swept.stale, swept.skipped, swept.more), (1, 0, false));
        assert_eq!(event_count(), before + 1, "one line for the swap");
        assert!(read_state(|state| state.store().auto_refund_waiting()).is_empty());
        assert!(verify_replay());
    });
}
