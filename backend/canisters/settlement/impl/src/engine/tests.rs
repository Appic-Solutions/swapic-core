use super::*;
use types::{Attempt, BasisPoints, Leg, Outcome, SwapStatus};

fn at(status: SwapStatus, last_leg: Option<Leg>, last_outcome: Option<Outcome>) -> Swap {
    Swap {
        quote_bytes: vec![],
        status,
        last_attempt: last_leg.map(|_| Attempt::FIRST),
        open_attempt: None,
        src_chain: ChainId::BASE,
        src_token: "usdc".parse().unwrap(),
        amount_in: TokenAmount::from(100_u8),
        amount_paid: None,
        waiting_since: None,
        last_leg,
        last_outcome,
        last_tx_hash: None,
        paid_out: None,
        fee_accrued: None,
        burn_max_fee: None,
    }
}

/// The engine's whole table, one row per reachable combination of status, latest leg and
/// its outcome. It is the law of which line follows which, so every row is spelled out.
#[test]
fn next_action_is_a_table_over_status_leg_and_outcome() {
    use Action::*;
    use Leg::{Burn, Mint, Payout, Reclaim as ReclaimLeg, Refund};
    use Outcome::*;
    use SwapStatus::*;
    let confirmed = Some(Confirmed);
    let failed = Some(Failed);
    let table: Vec<(SwapStatus, Option<Leg>, Option<Outcome>, Action)> = vec![
        // the funds are in the source vault and nothing has been signed: the rail's first leg
        (FundsReceived, None, None, Rail),
        // executing: the rail's own legs, until the stable is on the destination side
        (Executing, None, None, Rail),
        (Executing, Some(Burn), confirmed, Rail),
        (Executing, Some(Mint), confirmed, Rail),
        // a burn that reverted moved nothing, so the user is paid back
        (
            Executing,
            Some(Burn),
            failed,
            StartRefund("the burn reverted on the chain"),
        ),
        // a mint that reverted has the funds burned and not minted: a human
        (
            Executing,
            Some(Mint),
            failed,
            Freeze("the mint reverted on the chain"),
        ),
        // legs that are not the rail's, in a status that is the rail's
        (
            Executing,
            Some(Payout),
            confirmed,
            Freeze("a payout confirmed while executing"),
        ),
        (
            Executing,
            Some(Refund),
            confirmed,
            Freeze("a refund confirmed while executing"),
        ),
        (
            Executing,
            Some(ReclaimLeg),
            confirmed,
            Freeze("a reclaim confirmed while executing"),
        ),
        (
            Executing,
            Some(Payout),
            failed,
            Freeze("a payout failed while executing"),
        ),
        (
            Executing,
            Some(Refund),
            failed,
            Freeze("a refund failed while executing"),
        ),
        (
            Executing,
            Some(ReclaimLeg),
            failed,
            Freeze("a reclaim failed while executing"),
        ),
        // paid in stable: the user is paid out, whatever leg brought the stable in
        (PaidInStable, Some(Mint), confirmed, SendPayout),
        (PaidInStable, Some(Burn), confirmed, SendPayout),
        (PaidInStable, None, None, SendPayout),
        // delivering: the payout landed, or it did not
        (Delivering, Some(Payout), confirmed, RecordDone),
        (
            Delivering,
            Some(Payout),
            failed,
            Freeze("the payout reverted on the chain"),
        ),
        (
            Delivering,
            Some(Mint),
            confirmed,
            Freeze("delivering with no payout signed"),
        ),
        (
            Delivering,
            None,
            None,
            Freeze("delivering with no payout signed"),
        ),
        // refunding: paid back from the vault while the funds are in it, reclaimed first
        // when they are on the rail, and never when they have left for good
        (Refunding, None, None, SendRefund),
        (Refunding, Some(Burn), failed, SendRefund),
        (Refunding, Some(ReclaimLeg), confirmed, SendRefund),
        (Refunding, Some(Refund), confirmed, RecordRefunded),
        (Refunding, Some(Burn), confirmed, RailReclaim),
        (
            Refunding,
            Some(Refund),
            failed,
            Freeze("the refund reverted on the chain"),
        ),
        (
            Refunding,
            Some(ReclaimLeg),
            failed,
            Freeze("the reclaim reverted on the chain"),
        ),
        (
            Refunding,
            Some(Mint),
            confirmed,
            Freeze("the stable is on the destination side"),
        ),
        (
            Refunding,
            Some(Mint),
            failed,
            Freeze("the funds left for the rail and never arrived"),
        ),
        (
            Refunding,
            Some(Payout),
            confirmed,
            Freeze("the user was paid out"),
        ),
        (
            Refunding,
            Some(Payout),
            failed,
            Freeze("the user was paid out"),
        ),
        // nothing to do: the user is being asked, or the swap is closed
        (WaitingForUser, None, None, Wait),
        (WaitingForUser, Some(Burn), confirmed, Wait),
        (Done, Some(Payout), confirmed, Wait),
        (Refunded, Some(Refund), confirmed, Wait),
        (Frozen, Some(Mint), failed, Wait),
        (Frozen, None, None, Wait),
    ];
    for (status, leg, outcome, expected) in table {
        assert_eq!(
            next_action(&at(status, leg, outcome)),
            expected,
            "{status:?} after {leg:?} {outcome:?}"
        );
    }
}

/// An attempt that is still open is waited on, whatever else the swap says: the outbox
/// is deciding it against the chain.
#[test]
fn an_open_attempt_is_always_waited_on() {
    use SwapStatus::*;
    for status in [
        FundsReceived,
        Executing,
        PaidInStable,
        Delivering,
        Refunding,
    ] {
        for leg in [None, Some(Leg::Burn), Some(Leg::Payout), Some(Leg::Refund)] {
            let mut swap = at(status, leg, None);
            swap.open_attempt = Some(Attempt::FIRST);
            assert_eq!(next_action(&swap), Action::Wait, "{status:?} {leg:?}");
        }
    }
}

/// A closed leg whose outcome the fold never recorded is a state no line produces: it is
/// stopped for a human rather than stepped from.
#[test]
fn a_closed_leg_with_no_outcome_is_stopped() {
    use SwapStatus::*;
    for status in [Executing, Delivering, Refunding] {
        assert_eq!(
            next_action(&at(status, Some(Leg::Burn), None)),
            Action::Freeze("a leg closed with no outcome recorded"),
            "{status:?}"
        );
    }
}

/// The payout is what the stable brought in less the platform's fee, and never below the
/// least the user was quoted.
#[test]
fn the_payout_is_the_stable_less_the_fee_and_never_below_min_out() {
    let paid = TokenAmount::from(24_995_000_u32);
    let fee = |bps: u16| BasisPoints::new(bps);
    assert_eq!(
        payout_of(paid, fee(0), TokenAmount::from(24_900_000_u32)),
        Ok(Payout {
            amount: paid,
            fee: TokenAmount::ZERO,
        })
    );
    assert_eq!(
        payout_of(paid, fee(30), TokenAmount::from(24_900_000_u32)),
        Ok(Payout {
            amount: TokenAmount::from(24_920_015_u32),
            fee: TokenAmount::from(74_985_u32),
        }),
        "thirty basis points, rounded down in the platform's disfavour"
    );
    assert_eq!(
        payout_of(paid, fee(30), TokenAmount::from(24_990_000_u32)),
        Err(EngineError::BelowMinOut {
            payout: TokenAmount::from(24_920_015_u32),
            min_out: TokenAmount::from(24_990_000_u32),
        })
    );
    assert_eq!(
        payout_of(paid, fee(0), paid),
        Ok(Payout {
            amount: paid,
            fee: TokenAmount::ZERO,
        }),
        "exactly the minimum is enough"
    );
}

/// What the record says was paid is what the payout itself carried: the fee is the stable
/// less the amount the payout leg was signed for, so a config the operator moved between
/// the send and its confirmation changes nothing in the record, and a fee the live config
/// would now refuse cannot leave a confirmed payout unrecorded.
#[test]
fn the_record_reads_the_fee_off_the_payout_that_was_sent() {
    let paid = TokenAmount::from(24_995_000_u32);
    let sent = TokenAmount::from(24_920_015_u32);
    let mut swap = at(
        SwapStatus::Delivering,
        Some(Leg::Payout),
        Some(Outcome::Confirmed),
    );
    swap.amount_paid = Some(paid);
    swap.paid_out = Some(sent);
    assert_eq!(
        recorded_payout(&swap),
        Ok(Payout {
            amount: sent,
            fee: TokenAmount::from(74_985_u32),
        }),
        "the fee is what the payout left behind, whatever the config says now"
    );
    let no_fee = Swap {
        paid_out: Some(paid),
        ..swap.clone()
    };
    assert_eq!(
        recorded_payout(&no_fee),
        Ok(Payout {
            amount: paid,
            fee: TokenAmount::ZERO,
        })
    );
    // a payout leg whose amount the fold never read is a fold no line produces
    let unread = Swap {
        paid_out: None,
        ..swap.clone()
    };
    assert_eq!(recorded_payout(&unread), Err(EngineError::NoPayoutRecorded));
    // and one that paid more than arrived is not one either
    let impossible = Swap {
        paid_out: Some(TokenAmount::from(25_000_000_u32)),
        ..swap
    };
    assert_eq!(
        recorded_payout(&impossible),
        Err(EngineError::PayoutAboveStable {
            paid_out: TokenAmount::from(25_000_000_u32),
            paid,
        })
    );
}

/// Rule A7: one tick at a time, and the guard comes back when the tick ends, including
/// when it traps and unwinds through it.
#[test]
fn one_tick_runs_at_a_time_and_the_guard_comes_back_on_a_trap() {
    let first = TickGuard::take();
    assert!(first.is_some(), "the first tick takes the guard");
    assert!(
        TickGuard::take().is_none(),
        "a second tick finds the guard taken"
    );
    drop(first);
    assert!(
        TickGuard::take().is_some(),
        "the guard comes back when a tick ends"
    );

    let unwound = std::panic::catch_unwind(|| {
        let _guard = TickGuard::take().expect("the guard is free");
        panic!("the tick traps");
    });
    assert!(unwound.is_err());
    assert!(
        TickGuard::take().is_some(),
        "a trap releases the guard on the way out"
    );
}

/// The tick's window rotates: it starts after the swap the last tick ended on, takes at
/// most its cap, and wraps, so sixty open swaps under a cap of fifty are all driven in two
/// ticks and no swap behind a refusing one is starved.
#[test]
fn the_window_starts_after_the_last_swap_driven_and_wraps() {
    let swaps: Vec<(QuoteHash, Swap)> = (0..60)
        .map(|i| {
            (
                QuoteHash::new([i; 32]),
                at(SwapStatus::PaidInStable, None, None),
            )
        })
        .collect();
    let ids = |window: &[(QuoteHash, Swap)]| -> Vec<u8> {
        window.iter().map(|(hash, _)| hash.as_ref()[0]).collect()
    };
    // the swap a window ends on is the one the tick's cursor is left naming
    let last_of = |window: &[(QuoteHash, Swap)]| window.last().map(|(hash, _)| *hash);

    let first = window(&swaps, None, 50);
    assert_eq!(ids(&first), (0..50).collect::<Vec<u8>>());
    let after = last_of(&first).expect("a window that drove something ends on a swap");
    assert_eq!(after, QuoteHash::new([49; 32]));

    // the next tick starts after it and wraps at the end of the map
    let second = window(&swaps, Some(after), 50);
    assert_eq!(
        ids(&second),
        (50..60).chain(0..40).collect::<Vec<u8>>(),
        "the ten that waited first, then round again"
    );
    assert_eq!(
        last_of(&second),
        Some(QuoteHash::new([39; 32])),
        "and the tick after that carries on from there"
    );

    // a cursor naming a swap that has since closed is a place in the order, not a swap:
    // the window starts at the next one that is still open
    let closed = QuoteHash::new([44; 32]);
    let gone: Vec<(QuoteHash, Swap)> = swaps
        .iter()
        .filter(|(hash, _)| *hash != closed)
        .cloned()
        .collect();
    assert_eq!(
        window(&gone, Some(closed), 3).first().unwrap().0.as_ref()[0],
        45
    );

    // fewer swaps than the cap is one pass over all of them and no repeats
    assert_eq!(
        ids(&window(&swaps[..7], Some(QuoteHash::new([3; 32])), 50)).len(),
        7
    );
    assert!(window(&[], None, 50).is_empty());
}

/// The allocation of `purpose` on `chain` at `nonce`, carrying `data`: what
/// `create_and_send` appends before it signs.
fn allocated(purpose: TxPurpose, chain: ChainId, nonce: u64, data: Vec<u8>) -> EventType {
    EventType::TxCreated {
        purpose,
        chain_id: chain,
        nonce: types::Nonce::new(nonce),
        to: "0x2222222222222222222222222222222222222222"
            .parse()
            .unwrap(),
        value: Wei::ZERO,
        data,
        gas_limit: types::GasAmount::from(120_000_u32),
        max_fee: types::WeiPerGas::from(2_000_000_000_u64),
        max_priority_fee: types::WeiPerGas::from(100_000_000_u64),
    }
}

/// Attempt `attempt` of the swap, signed on `chain` and then confirmed there.
fn signed_and_confirmed(qh: QuoteHash, attempt: u32, chain: ChainId) -> [EventType; 2] {
    let tx_hash = TxHash::new([attempt as u8; 32]);
    [
        EventType::TxSigned {
            quote_hash: qh,
            attempt: Attempt::new(attempt),
            chain_id: chain,
            tx_hash,
            raw_tx: vec![],
        },
        EventType::TxConfirmed {
            quote_hash: qh,
            attempt: Attempt::new(attempt),
            chain_id: chain,
            tx_hash,
            block: types::BlockNumber::new(1),
        },
    ]
}

/// A fee is accrued once per swap. `record_done` writes the fee line ahead of `SwapDone`
/// as two appends, so a `SwapDone` refused after the fee line committed leaves the swap to
/// be recorded again on the next tick. That second time finds the fee already accrued for
/// the swap and writes only `SwapDone`, so the platform's fees count it once.
#[test]
fn a_fee_already_accrued_is_not_accrued_again() {
    use crate::state::transitions::tests::{funds, swap_id};
    use crate::storage::events::{append_event_at, event_count, read_state};
    use crate::storage::on_fresh_memory;
    on_fresh_memory(|| {
        let qh = swap_id(1);
        let payout = types::abi::vault_payout(
            qh,
            "0xaf88d065e77c8cC2239327C5EDb3A432268e5831"
                .parse()
                .unwrap(),
            "0x7551A66653f9a20979ed81835a0b7008EC83401b"
                .parse()
                .unwrap(),
            TokenAmount::from(97_u8),
        );
        let [burn_signed, burn_confirmed] = signed_and_confirmed(qh, 1, ChainId::BASE);
        let [payout_signed, payout_confirmed] = signed_and_confirmed(qh, 2, ChainId::ARBITRUM);
        let trail = [
            funds(1),
            allocated(TxPurpose::Burn(qh), ChainId::BASE, 0, vec![]),
            burn_signed,
            burn_confirmed,
            EventType::PaidInStable {
                quote_hash: qh,
                chain_id: ChainId::ARBITRUM,
                amount: TokenAmount::from(99_u8),
            },
            allocated(TxPurpose::Payout(qh), ChainId::ARBITRUM, 0, payout),
            payout_signed,
            payout_confirmed,
            // what an earlier `record_done` committed before its `SwapDone` was refused
            EventType::FeeAccrued {
                quote_hash: qh,
                amount: TokenAmount::from(2_u8),
            },
        ];
        for (n, event) in trail.into_iter().enumerate() {
            append_event_at(event, Timestamp::from_nanos(n as u64 + 1))
                .expect("the fold admits it");
        }
        let swap = read_state(|state| state.store().swap(&qh)).expect("the swap exists");
        assert_eq!(next_action(&swap), Action::RecordDone);
        let before = event_count();

        assert_eq!(
            record_done(qh, &swap, Timestamp::from_nanos(100)),
            Ok(Did::Recorded)
        );
        assert_eq!(
            read_state(|state| state.meta().fees_accrued),
            TokenAmount::from(2_u8),
            "the fee is accrued once"
        );
        assert_eq!(event_count(), before + 1, "one line: the swap done");
        assert_eq!(
            read_state(|state| state.store().swap(&qh)).map(|swap| swap.status),
            Some(SwapStatus::Done)
        );
    });
}

/// The swaps a rail the deploy has off holds are counted for the operator (E6): an open
/// swap on that rail whose next move is the rail's, a step or a reclaim, is paused, not
/// stopped, and moves again when the rail is back on. A swap on a rail that runs, one on
/// the paused rail whose next move is not the rail's (a refund out of the vault, which
/// needs no rail), and a closed one are not counted.
#[test]
fn the_swaps_a_disabled_rail_holds_are_counted() {
    use crate::state::transitions::tests::quote;
    use types::config::EcoEnabled;
    let on = |rail: types::Rail, swap: Swap| Swap {
        quote_bytes: Quote {
            rail,
            ..quote(swap.last_attempt.map_or(0, |attempt| attempt.get().into()))
        }
        .canonical_bytes()
        .unwrap(),
        ..swap
    };
    let swaps = [
        on(types::Rail::Eco, at(SwapStatus::FundsReceived, None, None)),
        on(
            types::Rail::Eco,
            at(
                SwapStatus::Refunding,
                Some(Leg::Burn),
                Some(Outcome::Confirmed),
            ),
        ),
        on(types::Rail::Eco, at(SwapStatus::Refunding, None, None)),
        on(types::Rail::Eco, at(SwapStatus::Done, None, None)),
        on(
            types::Rail::CctpV2Fast,
            at(SwapStatus::FundsReceived, None, None),
        ),
    ];
    let off = types::Config::default();
    assert_eq!(
        paused_on_disabled_rails(&off, swaps.iter()),
        2,
        "the step and the reclaim of the Eco swaps, and nothing else"
    );
    let running = types::Config {
        eco_enabled: EcoEnabled::ON,
        ..types::Config::default()
    };
    assert_eq!(paused_on_disabled_rails(&running, swaps.iter()), 0);
}

/// The paused count is read a page at a time (review 5, L6): a page reads at most its
/// limit of swaps, in swap id order, from the swap after the one the last page ended on,
/// and names the swap the next page starts after while any is left, so the query answers
/// however long the history grows. The pages together count what one read of every swap
/// would.
#[test]
fn the_paused_count_is_read_a_bounded_page_at_a_time() {
    use crate::state::transitions::tests::quote;
    use crate::state::MemoryStore;
    use types::Rail::{CctpV2Fast, Eco};
    let on = |rail: types::Rail, nonce: u64| Swap {
        quote_bytes: Quote {
            rail,
            ..quote(nonce)
        }
        .canonical_bytes()
        .unwrap(),
        ..at(SwapStatus::FundsReceived, None, None)
    };
    let id = |n: u8| QuoteHash::new([n; 32]);
    let mut store = MemoryStore::default();
    for (n, rail) in [
        (1, Eco),
        (2, CctpV2Fast),
        (3, Eco),
        (4, CctpV2Fast),
        (5, Eco),
    ] {
        store.put_swap(id(n), on(rail, n.into()));
    }
    let off = types::Config::default();
    assert_eq!(
        paused_page(&off, &store, None, 2),
        PausedPage {
            paused: 1,
            read: 2,
            next: Some(id(2)),
        }
    );
    assert_eq!(
        paused_page(&off, &store, Some(id(2)), 2),
        PausedPage {
            paused: 1,
            read: 2,
            next: Some(id(4)),
        }
    );
    assert_eq!(
        paused_page(&off, &store, Some(id(4)), 2),
        PausedPage {
            paused: 1,
            read: 1,
            next: None,
        },
        "the last page names no next one"
    );
    assert_eq!(
        paused_page(&off, &store, Some(id(3)), 2),
        PausedPage {
            paused: 1,
            read: 2,
            next: None,
        },
        "nor does a page that ends on the last swap"
    );
    assert_eq!(
        paused_on_disabled_rails(&off, store.swaps().iter().map(|(_, swap)| swap)),
        3,
        "the pages count what one read of every swap does"
    );
    assert_eq!(
        paused_page(&off, &store, None, 0),
        PausedPage {
            paused: 1,
            read: 1,
            next: Some(id(1)),
        },
        "a page reads at least one swap, so a walk always moves"
    );
    assert_eq!(MAX_PAUSED_PAGE, 500);
}
