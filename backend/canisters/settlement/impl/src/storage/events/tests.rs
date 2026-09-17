use super::*;
use crate::state::transitions::ReplayError;
use crate::state::MemoryStore;
use crate::storage::halt::is_halted;
use crate::storage::on_fresh_memory;
use crate::task_manager::expiry_sweep::run_expiry_sweep;
use crate::task_manager::replay_audit::run_replay_audit;
use types::events::Choice;
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
    let quote = QuoteHash::new([7; 32]);
    let amount = |value: u32| TokenAmount::from(value);
    let (stable, heap) = fold_both(vec![
        EventType::PocketFunded {
            chain_id: ChainId::BASE,
            amount: amount(1_000),
        },
        EventType::FundsReceived {
            quote_hash: quote,
            quote_bytes: vec![0xde, 0xad],
            chain_id: ChainId::BASE,
            token: "USDC".parse().unwrap(),
            amount: amount(100),
            tx_ref: "0xfeed".into(),
        },
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

fn funds(quote_hash: QuoteHash) -> EventType {
    EventType::FundsReceived {
        quote_hash,
        quote_bytes: vec![],
        chain_id: ChainId::BASE,
        token: "USDC".parse().unwrap(),
        amount: TokenAmount::from(1_u32),
        tx_ref: "0x".into(),
    }
}

fn append(payload: EventType) {
    append_event_at(payload, Timestamp::from_nanos(1)).expect("the stable fold admits it");
}

fn log_replay() -> Result<State<MemoryStore>, ReplayError> {
    EVENTS.with(|e| replay(e.borrow().iter()))
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
        assert!(is_halted(), "the audit halts instead of trapping");
    });
}

/// A pocket balance sits in the stable fold that no event produced, and the log spends it.
#[test]
fn replay_audit_halts_on_a_pocket_balance_no_event_produced() {
    on_fresh_memory(|| {
        let quote = QuoteHash::new([8; 32]);
        StableStore(()).put_pocket(
            ChainId::BASE,
            Pocket {
                available: TokenAmount::ZERO,
                reserved: TokenAmount::from(10_u32),
            },
        );
        append(funds(quote));
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
        assert!(is_halted());
    });
}

/// A divergence the log folds cleanly over is still a divergence: the comparison catches
/// what the replay admits.
#[test]
fn replay_audit_halts_on_a_fold_that_replays_cleanly_but_differs() {
    on_fresh_memory(|| {
        let quote = QuoteHash::new([6; 32]);
        append(funds(quote));
        let mut swap = StableStore(()).swap(&quote).unwrap();
        swap.status = SwapStatus::Frozen;
        StableStore(()).put_swap(quote, swap);

        assert!(log_replay().is_ok(), "every event is admissible");
        assert!(!verify_replay());
        run_replay_audit();
        assert!(is_halted());
    });
}

/// The waiting index as a scan of every swap would build it.
fn scanned_waiting(store: &impl Store) -> Vec<WaitingKey> {
    let mut keys: Vec<WaitingKey> = store
        .swaps()
        .into_iter()
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
        let [a, b, c, d] = [1, 2, 3, 4].map(|byte| QuoteHash::new([byte; 32]));
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
            (1, funds(a)),
            (2, funds(b)),
            (3, funds(c)),
            (4, funds(d)),
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
            (50, ask(a)),
        ];
        for (nanos, payload) in steps {
            append_event_at(payload.clone(), Timestamp::from_nanos(nanos))
                .expect("the fold admits it");
            let (index, scan) =
                read_state(|state| (state.store().waiting(), scanned_waiting(state.store())));
            assert_eq!(index, scan, "after {payload:?} at {nanos}");
            if nanos == 11 && payload == ask(c) {
                assert_eq!(
                    index,
                    vec![waiting_key(10, a), waiting_key(11, b), waiting_key(11, c)]
                );
            }
        }
        assert_eq!(
            read_state(|state| state.store().waiting()),
            vec![waiting_key(50, a)]
        );

        let heap = log_replay().expect("the log folds");
        assert_eq!(heap.store().waiting(), scanned_waiting(heap.store()));
        assert!(verify_replay(), "and the audit compares the two indexes");
    });
}

/// The index is part of the fold, so an entry no event made is a divergence the audit
/// halts on.
#[test]
fn replay_audit_halts_on_a_waiting_entry_no_event_made() {
    on_fresh_memory(|| {
        let quote = QuoteHash::new([5; 32]);
        append(funds(quote));
        assert!(verify_replay());
        StableStore(()).put_waiting(waiting_key(7, quote));

        assert!(!verify_replay());
        run_replay_audit();
        assert!(is_halted());
    });
}

/// The sweep finds timed-out swaps through the index alone: a waiting swap planted in the
/// swaps map with no index entry is invisible to it, which is what keeps a pass from
/// reading every swap ever recorded.
#[test]
fn the_expiry_sweep_reads_the_index_and_not_the_swaps() {
    on_fresh_memory(|| {
        let quote_hash = QuoteHash::new([4; 32]);
        append(funds(quote_hash));
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

        append(funds(QuoteHash::new([1; 32])));
        append(funds(QuoteHash::new([2; 32])));
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
            append_event_at(funds(QuoteHash::new([3; 32])), Timestamp::from_nanos(1)),
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
            append_event_at(funds(QuoteHash::new([3; 32])), Timestamp::from_nanos(1)),
            Err(AppendError::ChainDiverged { .. })
        ));
        assert_eq!(event_count(), 2, "a refused append writes nothing");
    });
}
