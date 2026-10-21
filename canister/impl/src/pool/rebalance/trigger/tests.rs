//! Review Focus 4: every rule of the triggers.

use super::*;
use types::hash::PoolRef;
use types::pool::payout::PayoutLines;
use types::pool::{Pocket, Pockets, PayoutQueue, PoolSettings, RebalanceLines};

fn usdc(whole: u64) -> TokenAmount {
    TokenAmount::from(whole * 1_000_000)
}

fn now() -> Timestamp {
    Timestamp::from_secs(1_800_000_000).unwrap()
}

/// A pool with settings and these free lines.
fn pool(base: u64, icp: u64) -> PoolLedger {
    PoolLedger {
        settings: Some(PoolSettings::default()),
        pockets: Pockets {
            base: Pocket {
                free: usdc(base),
                ..Pocket::default()
            },
            icp: Pocket {
                free: usdc(icp),
                ..Pocket::default()
            },
        },
        ..PoolLedger::default()
    }
}

fn settings() -> RebalanceSettings {
    RebalanceSettings::default()
}

fn open(direction: MoveDirection, amount: u64, left: bool) -> Move {
    let mut item = Move::opened(
        0,
        direction,
        usdc(amount),
        false,
        now(),
        None,
        TokenAmount::ZERO,
    );
    item.left = left;
    item
}

#[test]
fn inside_the_band_nothing_moves() {
    // 40% and 26% of $10,000 on ICP: within 25% to 55%
    assert_eq!(decide(&pool(6_000, 4_000), &[], &settings(), now()), None);
    assert_eq!(decide(&pool(7_400, 2_600), &[], &settings(), now()), None);
    assert_eq!(decide(&pool(4_500, 5_500), &[], &settings(), now()), None);
    // no pool settings, no move
    let mut bare = pool(9_000, 1_000);
    bare.settings = None;
    assert_eq!(decide(&bare, &[], &settings(), now()), None);
}

#[test]
fn a_drift_out_of_the_band_moves_the_pocket_back_to_the_target() {
    // 20% on ICP: $2,000 to ICP brings it to 40%
    assert_eq!(
        decide(&pool(8_000, 2_000), &[], &settings(), now()),
        Some(Want {
            direction: MoveDirection::BaseToIcp,
            amount: usdc(2_000),
            urgent: false,
        })
    );
    // 60% on ICP: $2,000 to Base
    assert_eq!(
        decide(&pool(4_000, 6_000), &[], &settings(), now()),
        Some(Want {
            direction: MoveDirection::IcpToBase,
            amount: usdc(2_000),
            urgent: false,
        })
    );
}

#[test]
fn a_drift_below_the_least_move_waits() {
    // 20% of $1,000: $200 to the target, below $300
    assert_eq!(decide(&pool(800, 200), &[], &settings(), now()), None);
    // with a lower least move it goes
    let low = RebalanceSettings {
        min_move: usdc(100),
        ..settings()
    };
    assert_eq!(
        decide(&pool(800, 200), &[], &low, now()).map(|want| want.amount),
        Some(usdc(200))
    );
}

#[test]
fn a_move_is_capped_by_max_move_the_day_and_the_pocket_behind_its_queue() {
    // 10% of $100,000 on ICP: $30,000 to the target, capped at $5,000
    let big = pool(90_000, 10_000);
    assert_eq!(
        decide(&big, &[], &settings(), now()).map(|want| want.amount),
        Some(usdc(5_000))
    );
    // $16,000 opened within the day: $4,000 left of the $20,000 cap
    let mut tired = big.clone();
    tired.rebalance = Some(RebalanceLines {
        recent: vec![(now(), usdc(16_000))],
        ..RebalanceLines::default()
    });
    assert_eq!(
        decide(&tired, &[], &settings(), now()).map(|want| want.amount),
        Some(usdc(4_000))
    );
    // the day full: nothing
    tired.rebalance = Some(RebalanceLines {
        recent: vec![(now(), usdc(20_000))],
        ..RebalanceLines::default()
    });
    assert_eq!(decide(&tired, &[], &settings(), now()), None);
    // a day later it is free again
    let later = now().checked_add(std::time::Duration::from_secs(86_401)).unwrap();
    assert!(decide(&tired, &[], &settings(), later).is_some());
    // the Base queue keeps its USDC: only $800 is available to move
    let mut queued = pool(8_000, 2_000);
    queued.payouts = Some(PayoutLines {
        queued: usdc(7_200),
        queue_len: 1,
        ..PayoutLines::default()
    });
    queued.pockets.base.free = usdc(8_000);
    assert_eq!(
        decide(&queued, &[], &settings(), now()).map(|want| want.amount),
        Some(usdc(800))
    );
}

#[test]
fn never_a_second_move_in_the_same_direction() {
    // a $2,000 move to ICP open: the split reads it arrived, nothing more
    let moving = [open(MoveDirection::BaseToIcp, 2_000, true)];
    let mut ledger = pool(8_000, 2_000);
    ledger.pockets.base.free = usdc(6_000);
    ledger.pockets.base.in_flight = usdc(2_000);
    assert_eq!(decide(&ledger, &moving, &settings(), now()), None);
    // a $500 one open the same way, still short of the band: nothing either
    let small = [open(MoveDirection::BaseToIcp, 500, false)];
    let mut ledger = pool(9_500, 1_000);
    ledger.pockets.base.reserved = usdc(500);
    assert_eq!(decide(&ledger, &small, &settings(), now()), None);
}

#[test]
fn the_split_counts_an_open_move_at_its_destination() {
    let ledger = {
        let mut ledger = pool(7_000, 2_000);
        ledger.pockets.base.reserved = usdc(1_000);
        ledger
    };
    // reserved on Base, not left: counted on ICP only
    assert_eq!(
        split(&ledger, &[open(MoveDirection::BaseToIcp, 1_000, false)]),
        (3_000_000_000, 7_000_000_000)
    );
    // left, in flight: in no line of Base, counted on ICP
    let mut flying = pool(7_000, 2_000);
    flying.pockets.base.in_flight = usdc(1_000);
    assert_eq!(
        split(&flying, &[open(MoveDirection::BaseToIcp, 1_000, true)]),
        (3_000_000_000, 7_000_000_000)
    );
}

#[test]
fn a_waiting_icp_payout_moves_at_once_whatever_the_least_move() {
    // $100 free on ICP, a $250 payout queued: urgent, no least move
    let mut ledger = pool(600, 100);
    ledger.queue = PayoutQueue(vec![(PoolRef::new([1; 32]), usdc(250))]);
    let want = decide(&ledger, &[], &settings(), now()).unwrap();
    assert_eq!(want.direction, MoveDirection::BaseToIcp);
    assert!(want.urgent);
    // the queue's $150 and the way back to 40% of the $450 left: $180 less the -$150
    assert!(want.amount >= usdc(150));
    assert_eq!(want.amount, usdc(330));
    // a move to ICP already open: none more
    let moving = [open(MoveDirection::BaseToIcp, 200, false)];
    ledger.pockets.base.free = usdc(400);
    ledger.pockets.base.reserved = usdc(200);
    assert_eq!(decide(&ledger, &moving, &settings(), now()), None);
}

#[test]
fn a_waiting_base_payout_moves_icp_to_base_at_once() {
    // $5,000 on ICP, $1,000 free on Base, a $1,500 payout queued there
    let mut ledger = pool(1_000, 5_000);
    ledger.payouts = Some(PayoutLines {
        queued: usdc(1_500),
        queue_len: 1,
        ..PayoutLines::default()
    });
    let want = decide(&ledger, &[], &settings(), now()).unwrap();
    assert_eq!(want.direction, MoveDirection::IcpToBase);
    assert!(want.urgent);
    assert!(want.amount >= usdc(500));
    // 60% of the $4,500 left on Base is $2,700: $3,200 from ICP
    assert_eq!(want.amount, usdc(3_200));
}
