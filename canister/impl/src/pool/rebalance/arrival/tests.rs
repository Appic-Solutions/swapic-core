//! Review Focus 2: a move's Base arrival is the move's, under its own ref, credited once and
//! at most its amount; never a deposit's or an orphan while its own transaction closes. An
//! unfilled Eco intent's refund routes its move again.

use super::*;
use crate::state::MemoryStore;
use types::pool::{CreditKey, Move, PoolLedger, RebalanceLines};
use types::{BlockNumber, Timestamp, TokenAmount};

fn usdc(whole: u64) -> TokenAmount {
    TokenAmount::from(whole * 1_000_000)
}

fn key() -> CreditKey {
    CreditKey {
        chain: ChainId::BASE,
        tx: TxHash::new([0x81; 32]),
        log_index: 4,
    }
}

fn found(amount: TokenAmount, proof: Proof) -> Found {
    Found {
        credit: key(),
        amount,
        block: BlockNumber::new(37_000_000),
        at: None,
        proof,
        rail: types::pool::PoolRail::Cctp,
    }
}

fn state_with(item: Move) -> State<MemoryStore> {
    let mut store = MemoryStore::default();
    store.put_pool_ledger(PoolLedger {
        rebalance: Some(RebalanceLines {
            open: [item.id].into_iter().collect(),
            ..RebalanceLines::default()
        }),
        ..PoolLedger::default()
    });
    store.put_pool_move(item.id, item);
    State::new(store)
}

fn up(status: MoveStatus) -> Move {
    let mut item = Move::opened(
        3,
        MoveDirection::IcpToBase,
        usdc(1_000),
        false,
        Timestamp::from_nanos(1),
        None,
        TokenAmount::ZERO,
    );
    item.status = status;
    item.left = true;
    // its own route, fast: Circle keeps 970 at the most
    item.route = Some(types::pool::MoveRoute::Own {
        max_fee: TokenAmount::from(970_u64),
        fast: true,
    });
    item
}

#[test]
fn a_mint_under_the_moves_ref_is_its_arrival_at_most_its_amount() {
    let state = state_with(up(MoveStatus::Burned));
    let deposit = |amount| {
        found(
            amount,
            Proof::Deposit {
                r#ref: move_ref(3),
                payer: None,
            },
        )
    };
    // CCTP fast kept 970: what arrived is credited
    assert_eq!(
        step_for(&state, &deposit(TokenAmount::from(999_999_030_u64))),
        Some(PoolStep::MoveArrived {
            id: 3,
            credited: TokenAmount::from(999_999_030_u64),
            credit: Some(key()),
            block: Some(BlockNumber::new(37_000_000)),
            ledger_block: None,
        })
    );
    // never more than the move
    assert!(matches!(
        step_for(&state, &deposit(usdc(1_200))),
        Some(PoolStep::MoveArrived { credited, .. }) if credited == usdc(1_000)
    ));
    // another ref is the deposits' to judge
    let other = found(
        usdc(1_000),
        Proof::Deposit {
            r#ref: move_ref(4),
            payer: None,
        },
    );
    assert_eq!(step_for(&state, &other), None);
    assert!(!waits_for_its_move(&state, &other));
}

/// A move whose own transaction has not closed yet is not credited, and its arrival is no
/// deposit's: the canister's own read leaves it for the watcher's report.
#[test]
fn an_arrival_before_its_moves_close_waits_and_is_never_an_orphan() {
    let state = state_with(up(MoveStatus::Signed(9)));
    let early = found(
        usdc(1_000),
        Proof::Deposit {
            r#ref: move_ref(3),
            payer: None,
        },
    );
    assert_eq!(step_for(&state, &early), None);
    assert!(waits_for_its_move(&state, &early));
}

#[test]
fn an_eco_refund_of_a_moves_intent_routes_it_again() {
    let mut down = Move::opened(
        5,
        MoveDirection::BaseToIcp,
        usdc(1_000),
        false,
        Timestamp::from_nanos(1),
        None,
        TokenAmount::ZERO,
    );
    down.status = MoveStatus::Burned;
    down.route = Some(types::pool::MoveRoute::Proposed {
        fee: TokenAmount::from(100_000_u64),
    });
    down.intent = Some([0x82; 32]);
    let state = state_with(down);
    let refund = |amount, intent| {
        found(
            amount,
            Proof::Refund {
                intents: vec![intent],
            },
        )
    };
    assert_eq!(
        step_for(&state, &refund(usdc(1_000), [0x82; 32])),
        Some(PoolStep::MoveRefunded {
            id: 5,
            credit: key(),
            amount: usdc(1_000),
            block: BlockNumber::new(37_000_000),
        })
    );
    // another intent, or another amount, is not the move's
    assert_eq!(step_for(&state, &refund(usdc(1_000), [0x83; 32])), None);
    assert_eq!(step_for(&state, &refund(usdc(999), [0x82; 32])), None);
}

/// Review Focus 2 and 5: the ref is public, and anyone's order may carry it. A stranger's $1
/// under the move's ref credits nothing and becomes no orphan; the move's own arrival after
/// it is credited.
#[test]
fn a_strangers_small_transfer_under_the_moves_ref_credits_nothing() {
    // its own route offered Circle 970 at the most: it carries at least $999.99903
    let state = state_with(up(MoveStatus::Burned));
    let under = |amount| {
        found(
            amount,
            Proof::Deposit {
                r#ref: move_ref(3),
                payer: None,
            },
        )
    };
    let dollar = under(usdc(1));
    assert_eq!(step_for(&state, &dollar), None);
    assert!(waits_for_its_move(&state, &dollar));
    let short = under(TokenAmount::from(999_999_029_u64));
    assert_eq!(step_for(&state, &short), None);
    // the move's own mint, Circle having kept 970
    assert!(matches!(
        step_for(&state, &under(TokenAmount::from(999_999_030_u64))),
        Some(PoolStep::MoveArrived { credited, .. })
            if credited == TokenAmount::from(999_999_030_u64)
    ));
}

/// P11e (a HIGH of the collected list): an Eco refund routes a move again only once the
/// intent's destination Portal shows it never filled; filled, it is held for a human; not
/// read, it waits.
mod refunds_proven {
    use super::*;
    use crate::pool::arrival::{prove_refunds, FillReads};
    use crate::storage::on_fresh_memory;
    use crate::storage::pool::record_at;
    use crate::storage::pool::tests::{base_stake, created, funded_pool};
    use types::eco::Fill;
    use types::events::TxPurpose;
    use types::pool::payout::PinnedOrder;
    use types::pool::{MoveRoute, RebalanceSettings};
    use types::Nonce;

    struct Portal(Result<Fill, String>);

    impl FillReads for Portal {
        async fn fill(
            &self,
            chain: ChainId,
            _intent: [u8; 32],
            route_deadline: u64,
        ) -> Result<Fill, String> {
            assert_eq!(
                chain,
                ChainId::ETHEREUM,
                "a move to ICP is filled on Ethereum"
            );
            assert_eq!(
                route_deadline, ROUTE_DEADLINE,
                "read past the deadline of the route the move's order published"
            );
            self.0.clone()
        }
    }

    /// The last instant the move's Eco route may be filled.
    const ROUTE_DEADLINE: u64 = 1_800_001_800;

    /// An hour past the route's deadline: an agreed block past it can be read.
    fn late() -> Timestamp {
        Timestamp::from_secs(ROUTE_DEADLINE + 3_600).unwrap()
    }

    /// The last review (item 3): a refund's proof reads the Portal only once a block every
    /// node agrees on can be past the route's deadline (`agreed::fill_wait`, the payouts'
    /// own rule); before it, it is too early and nothing is read.
    #[test]
    fn a_refund_is_not_read_before_an_agreed_block_can_be_past_its_deadline() {
        on_fresh_memory(|| {
            eco_move();
            let early = Timestamp::from_secs(ROUTE_DEADLINE + 10).unwrap();
            let proven = block_on(prove_refunds(
                refund(INTENT),
                &Portal(Err("read".to_string())),
                early,
            ));
            // a quiet wait for its time, no alert (the review, 2026-10-24)
            assert!(
                matches!(&proven[0].proof, Proof::TooEarly { from } if *from > ROUTE_DEADLINE),
                "{:?}",
                proven[0].proof
            );
        });
    }

    /// The move's order: one `publishAndFund` delivering USDC on Ethereum, its route's
    /// deadline [`ROUTE_DEADLINE`].
    fn eco_order() -> Vec<u8> {
        crate::storage::pool::tests::eco_order(ROUTE_DEADLINE)
    }

    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        let mut future = std::pin::pin!(future);
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        match future.as_mut().poll(&mut context) {
            std::task::Poll::Ready(out) => out,
            std::task::Poll::Pending => panic!("the fake never waits"),
        }
    }

    const INTENT: [u8; 32] = [0x84; 32];

    /// A $1,000 move to ICP routed by Eco, its intent published.
    fn eco_move() {
        crate::storage::init();
        funded_pool(usdc(4_000));
        base_stake(
            "0x1111111111111111111111111111111111111111"
                .parse()
                .unwrap(),
            usdc(6_000),
            0x53,
            9,
        );
        let at = |n: u64| Timestamp::from_secs(1_800_000_000 + n).unwrap();
        let fee = TokenAmount::from(100_000_u64);
        assert_eq!(
            crate::pool::rebalance::drive::eco_route_deadline(&eco_order()),
            Some(ROUTE_DEADLINE)
        );
        let order = PinnedOrder {
            order_id: [0x85; 32],
            order: eco_order(),
            gas_limit: 300_000,
            deadline_s: 1_800_003_600,
        };
        let steps = [
            PoolStep::MoveSettingsSet {
                settings: RebalanceSettings::default(),
            },
            PoolStep::MoveOpened {
                id: 0,
                direction: MoveDirection::BaseToIcp,
                amount: usdc(1_000),
                urgent: false,
                opened_at: at(1),
                window_ends: Some(at(61)),
                own_cost: usdc(1),
            },
            PoolStep::MoveProposed {
                id: 0,
                order: order.clone(),
                fee,
            },
            PoolStep::MoveRouted {
                id: 0,
                route: MoveRoute::Proposed { fee },
                order,
                scan_from: None,
            },
        ];
        for step in steps {
            record_at(step, at(2)).unwrap();
        }
        crate::storage::events::append_event_at(created(TxPurpose::PoolMove(0), 0), at(3)).unwrap();
        for step in [
            PoolStep::MoveSigned {
                id: 0,
                chain_id: ChainId::BASE,
                nonce: Nonce::new(0),
                tx_hash: TxHash::new([0x86; 32]),
                raw_tx: vec![2],
            },
            PoolStep::MoveBurned {
                id: 0,
                tx_hash: TxHash::new([0x86; 32]),
                block: BlockNumber::new(50),
                intent: Some(INTENT),
            },
        ] {
            record_at(step, at(4)).unwrap();
        }
    }

    fn refund(intent: [u8; 32]) -> Vec<Found> {
        vec![found(
            usdc(1_000),
            Proof::Refund {
                intents: vec![intent],
            },
        )]
    }

    #[test]
    fn a_refund_of_an_intent_never_filled_routes_the_move_again() {
        on_fresh_memory(|| {
            eco_move();
            for fill in [Fill::Open, Fill::Cancelled] {
                let proven = block_on(prove_refunds(refund(INTENT), &Portal(Ok(fill)), late()));
                assert_eq!(
                    proven[0].proof,
                    Proof::Refund {
                        intents: vec![INTENT]
                    }
                );
                assert!(matches!(
                    read_state(|state| step_for(state, &proven[0])),
                    Some(PoolStep::MoveRefunded { id: 0, .. })
                ));
            }
        });
    }

    #[test]
    fn a_refund_of_a_filled_intent_is_held_and_an_unread_one_waits() {
        on_fresh_memory(|| {
            eco_move();
            let filled = block_on(prove_refunds(
                refund(INTENT),
                &Portal(Ok(Fill::Filled)),
                late(),
            ));
            assert!(
                matches!(&filled[0].proof, Proof::Orphan { why, .. } if why.contains("filled"))
            );
            assert_eq!(read_state(|state| step_for(state, &filled[0])), None);
            let unread = block_on(prove_refunds(
                refund(INTENT),
                &Portal(Err("the read failed".to_string())),
                late(),
            ));
            assert!(matches!(&unread[0].proof, Proof::Unproven { .. }));
            // an intent no payout or move took: left to the fold's own rule, no read made
            let theirs = block_on(prove_refunds(
                refund([0x87; 32]),
                &Portal(Err("never read".to_string())),
                late(),
            ));
            assert_eq!(
                theirs[0].proof,
                Proof::Refund {
                    intents: vec![[0x87; 32]]
                }
            );
        });
    }
}
