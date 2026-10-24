use super::*;
use crate::state::transitions::tests::{funds, funds_manual, manual_swap_id, swap_id};
use crate::state::transitions::ReplayError;
use crate::state::MemoryStore;
use crate::storage::audit_cursor;
use crate::storage::halt::is_halted;
use crate::storage::on_fresh_memory;
use crate::task_manager::replay_audit::{run_audit_replay_step, run_replay_audit};
use types::config::AuditChunk;
use types::events::{Choice, TxPurpose};
use types::quote::QuoteError;
use types::LedgerMeta;
use types::{
    Attempt, GasAmount, Quote, SwapStatus, TokenAmount, TxHash, WaitingKey, Wei, WeiPerGas,
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
        created(quote),
        EventType::TxSigned {
            quote_hash: quote,
            attempt: Attempt::FIRST,
            chain_id: ChainId::BASE,
            tx_hash: TxHash::new([1; 32]),
            raw_tx: vec![],
        },
    ]);
    assert!(heap.matches(&stable));
    assert_eq!(stable.meta().next_event_index, EventIndex::new(5));
    assert_eq!(stable.pocket(&ChainId::BASE).unwrap().reserved, amount(400));
    assert_eq!(
        stable.swap(&quote).unwrap().open_attempt,
        Some(Attempt::FIRST)
    );
}

fn append(payload: EventType) {
    append_event_at(payload, Timestamp::from_nanos(1)).expect("the stable fold admits it");
}

/// The allocation the first signed record of `quote` spends: nonce zero on Base, which is
/// where every chain's allocator starts.
fn created(quote: QuoteHash) -> EventType {
    EventType::TxCreated {
        purpose: TxPurpose::Payout(quote),
        chain_id: ChainId::BASE,
        nonce: Nonce::ZERO,
        to: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"
            .parse()
            .unwrap(),
        value: Wei::ZERO,
        data: vec![],
        gas_limit: GasAmount::from(120_000_u32),
        max_fee: WeiPerGas::from(2_000_000_000_u64),
        max_priority_fee: WeiPerGas::from(100_000_000_u64),
        action: None,
    }
}

fn log_replay() -> Result<State<MemoryStore>, ReplayError> {
    EVENTS.with(|e| replay(e.borrow().iter()))
}

/// The deep check ops run by hand, in one step the size of the log: the comparison
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
            last_leg: None,
            last_outcome: None,
            last_tx_hash: None,
            paid_out: None,
            fee_accrued: None,
            burn_max_fee: None,
        };
        StableStore(()).put_swap(quote, ghost);
        // the stable fold holds the ghost, so it admits the allocation and the signed
        // record; the replay of the log, which never funded the swap, admits neither
        append(created(quote));
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
            reason: "ops".into(),
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

/// Both stores read their swaps a page at a time (review 5, L6): at most the limit, in
/// swap id order, from the first swap after the one named, whether or not a swap holds
/// that id, and from the first of all when none is named. A page past the last swap is
/// empty.
#[test]
fn the_swaps_are_read_a_page_at_a_time() {
    on_fresh_memory(|| {
        crate::storage::init();
        let id = |n: u8| QuoteHash::new([n; 32]);
        let swap = Swap {
            quote_bytes: vec![],
            status: SwapStatus::FundsReceived,
            last_attempt: None,
            open_attempt: None,
            src_chain: ChainId::BASE,
            src_token: "USDC".parse().unwrap(),
            amount_in: TokenAmount::from(1_u32),
            amount_paid: None,
            waiting_since: None,
            last_leg: None,
            last_outcome: None,
            last_tx_hash: None,
            paid_out: None,
            fee_accrued: None,
            burn_max_fee: None,
        };
        let mut heap = MemoryStore::default();
        for n in [2, 4, 6, 8, 10] {
            StableStore(()).put_swap(id(n), swap.clone());
            heap.put_swap(id(n), swap.clone());
        }
        let ids = |page: Vec<(QuoteHash, Swap)>| -> Vec<QuoteHash> {
            page.into_iter().map(|(quote_hash, _)| quote_hash).collect()
        };
        let pages = [
            (None, 2, vec![id(2), id(4)]),
            (Some(id(4)), 2, vec![id(6), id(8)]),
            (Some(id(5)), 2, vec![id(6), id(8)]),
            (Some(id(8)), 2, vec![id(10)]),
            (Some(id(10)), 2, vec![]),
            (None, 500, vec![id(2), id(4), id(6), id(8), id(10)]),
        ];
        for (after, limit, expected) in pages {
            assert_eq!(
                ids(StableStore(()).swaps_after(after, limit)),
                expected,
                "stable, after {after:?}"
            );
            assert_eq!(
                ids(heap.swaps_after(after, limit)),
                expected,
                "heap, after {after:?}"
            );
        }
    });
}
