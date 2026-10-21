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
