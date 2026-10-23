//! The driver's steps (P11c Task 4): what a move does next, and the ICP legs on a fake ledger
//! and minter: the minter's call with its approvals, and the sweep, credited at most the
//! move's amount (a donation is never the pool's).

use super::*;
use crate::storage::on_fresh_memory;
use crate::storage::pool::tests::{base_stake, funded_pool, usdc, FEE};
use crate::storage::pool::{ledger, record_at};
use std::cell::RefCell;
use types::events::TxPurpose;
use types::pool::RebalanceSettings;

fn at(secs: u64) -> Timestamp {
    Timestamp::from_secs(1_800_000_000 + secs).unwrap()
}

fn item(direction: MoveDirection, status: MoveStatus) -> Move {
    let mut item = Move::opened(
        1,
        direction,
        usdc(500),
        false,
        at(0),
        Some(at(60)),
        TokenAmount::ZERO,
    );
    item.status = status;
    item
}

#[test]
fn a_move_to_icp_is_routed_when_its_window_ends_and_an_urgent_one_at_once() {
    let open = item(MoveDirection::BaseToIcp, MoveStatus::Open);
    assert_eq!(next(&open, at(30), false), Next::Wait);
    assert_eq!(next(&open, at(60), false), Next::Route);
    let urgent = Move {
        urgent: true,
        window_ends: None,
        ..open.clone()
    };
    assert_eq!(next(&urgent, at(0), false), Next::Route);
    // stuck, final or with a transaction out: nothing
    let stuck = Move {
        stuck: Some("a test".to_string()),
        ..open.clone()
    };
    assert_eq!(next(&stuck, at(90), false), Next::Wait);
    assert_eq!(next(&open, at(90), true), Next::Wait);
}

#[test]
fn a_move_to_base_asks_the_minter_then_reads_its_landing_then_takes_its_window() {
    let open = Move {
        window_ends: None,
        ..item(MoveDirection::IcpToBase, MoveStatus::Open)
    };
    assert_eq!(next(&open, at(0), false), Next::Withdraw);
    // a minter's call whose answer was lost: a controller reads the minter
    let withdrawing = Move {
        status: MoveStatus::Withdrawing,
        ..open.clone()
    };
    assert!(matches!(next(&withdrawing, at(0), false), Next::Stuck(_)));
    let withdrawn = Move {
        status: MoveStatus::Withdrawn,
        ..open.clone()
    };
    assert_eq!(next(&withdrawn, at(600), false), Next::Landing);
    assert!(matches!(
        next(&withdrawn, at(3 * 3_600), false),
        Next::Stuck(_)
    ));
    let landed = Move {
        status: MoveStatus::Landed,
        window_ends: Some(at(1_260)),
        since: at(1_200),
        ..open
    };
    assert_eq!(next(&landed, at(1_230), false), Next::Wait);
    assert_eq!(next(&landed, at(1_260), false), Next::Route);
}

#[test]
fn a_routed_move_signs_its_pinned_order_again_only_within_its_deadline_and_reverts() {
    let mut routed = item(MoveDirection::BaseToIcp, MoveStatus::Routed);
    routed.order = Some(PinnedOrder {
        order_id: [1; 32],
        order: vec![],
        gas_limit: 300_000,
        deadline_s: at(3_600).as_secs().get(),
    });
    assert_eq!(next(&routed, at(0), false), Next::Sign { again: false });
    routed.tx_hash = Some(TxHash::new([2; 32]));
    routed.reverts = Some(1);
    assert_eq!(next(&routed, at(0), false), Next::Sign { again: true });
    routed.reverts = Some(MAX_REVERTS);
    assert!(matches!(next(&routed, at(0), false), Next::Stuck(_)));
    routed.reverts = Some(1);
    assert!(matches!(next(&routed, at(3_500), false), Next::Stuck(_)));
}

#[test]
fn a_burned_move_to_icp_sweeps_or_looks_for_its_eco_fill() {
    let mut own = item(MoveDirection::BaseToIcp, MoveStatus::Burned);
    own.route = Some(MoveRoute::Own {
        max_fee: TokenAmount::ZERO,
        fast: false,
    });
    assert_eq!(next(&own, at(600), false), Next::Sweep);
    assert!(matches!(next(&own, at(3 * 3_600), false), Next::Stuck(_)));
    let mut eco = own.clone();
    eco.route = Some(MoveRoute::Proposed {
        fee: TokenAmount::from(100_u64),
    });
    assert_eq!(next(&eco, at(60), false), Next::Fill);
    eco.landed = Some(types::pool::Landing {
        tx: TxHash::new([3; 32]),
        block: BlockNumber::new(1),
        amount: usdc(499),
    });
    assert_eq!(next(&eco, at(60), false), Next::Sweep);
    let landed = Move {
        status: MoveStatus::Landed,
        ..eco.clone()
    };
    assert_eq!(next(&landed, at(60), false), Next::Deposit);
    let sweeping = Move {
        status: MoveStatus::Sweeping,
        ..eco
    };
    assert_eq!(next(&sweeping, at(60), false), Next::Resweep);
    // a move to Base waits on Base for the watcher's report, and stops after hours
    let up = item(MoveDirection::IcpToBase, MoveStatus::Burned);
    assert_eq!(next(&up, at(600), false), Next::Wait);
    assert!(matches!(next(&up, at(3 * 3_600), false), Next::Stuck(_)));
}

#[test]
fn a_sweep_credits_at_most_what_the_move_may_carry() {
    let fee = TokenAmount::from(FEE);
    // nothing until the move's ckUSDC is all there
    assert_eq!(swept(usdc(499), fee, usdc(500)), None);
    // all of it less the fee moves; the fee is the move's cost
    assert_eq!(
        swept(usdc(500), fee, usdc(500)),
        Some((
            usdc(500).checked_sub(fee).unwrap(),
            usdc(500).checked_sub(fee).unwrap()
        ))
    );
    // a donation moves with it, and is never credited
    assert_eq!(
        swept(usdc(900), fee, usdc(500)),
        Some((usdc(900).checked_sub(fee).unwrap(), usdc(500)))
    );
}

/// What the fake was asked, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Asked {
    Balance(Principal, Account),
    Approve(Principal, Principal, Nat, [u8; 32], u64),
    Transfer(Option<[u8; 32]>, Account, Nat, [u8; 32], u64),
    Withdraw(Principal, Nat, String),
}

#[derive(Default)]
struct Fake {
    asked: RefCell<Vec<Asked>>,
    balance: Nat,
    withdraw: Option<Result<u64, CallError>>,
    transfer: Option<Result<u64, CallError>>,
    status: Option<ic::WithdrawalStatus>,
}

impl IcpCalls for Fake {
    async fn balance(&self, ledger: Principal, account: Account) -> Result<Nat, CallError> {
        self.asked
            .borrow_mut()
            .push(Asked::Balance(ledger, account));
        Ok(self.balance.clone())
    }

    async fn approve(
        &self,
        ledger: Principal,
        spender: Principal,
        amount: Nat,
        dedup: Dedup,
    ) -> Result<(), CallError> {
        self.asked.borrow_mut().push(Asked::Approve(
            ledger,
            spender,
            amount,
            dedup.memo,
            dedup.created_at_nanos,
        ));
        Ok(())
    }

    async fn transfer(
        &self,
        _ledger: Principal,
        from_subaccount: Option<[u8; 32]>,
        to: Account,
        amount: Nat,
        dedup: Dedup,
    ) -> Result<u64, CallError> {
        self.asked.borrow_mut().push(Asked::Transfer(
            from_subaccount,
            to,
            amount,
            dedup.memo,
            dedup.created_at_nanos,
        ));
        self.transfer.clone().unwrap_or(Ok(77))
    }

    async fn withdraw_erc20(
        &self,
        _minter: Principal,
        ledger: Principal,
        amount: Nat,
        recipient: String,
    ) -> Result<ic::RetrieveErc20Request, CallError> {
        self.asked
            .borrow_mut()
            .push(Asked::Withdraw(ledger, amount, recipient));
        self.withdraw
            .clone()
            .unwrap_or(Ok(42))
            .map(|id| ic::RetrieveErc20Request {
                cketh_block_index: Nat::from(id),
                ckerc20_block_index: Nat::from(id + 1),
            })
    }

    async fn withdrawal_status(
        &self,
        minter: Principal,
        id: u64,
    ) -> Result<Vec<ic::WithdrawalDetail>, CallError> {
        Ok(self
            .status
            .clone()
            .map(|status| ic::WithdrawalDetail {
                withdrawal_id: id,
                recipient_address: String::new(),
                from: minter,
                from_subaccount: None,
                token_symbol: "ckUSDC".to_string(),
                withdrawal_amount: Nat::from(0_u8),
                max_transaction_fee: None,
                status,
            })
            .into_iter()
            .collect())
    }
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    match future.as_mut().poll(&mut context) {
        std::task::Poll::Ready(out) => out,
        std::task::Poll::Pending => panic!("the fake never waits"),
    }
}

fn minter() -> Principal {
    Principal::from_slice(&[0x61; 10])
}

fn cketh() -> Principal {
    Principal::from_slice(&[0x62; 10])
}

fn ctx() -> Ctx {
    Ctx {
        now: at(100),
        principal: Principal::from_slice(&[0x63; 10]),
        canister: "0x36D34038F04990a58c068e08eD9754e2E3D9bA53"
            .parse()
            .unwrap(),
    }
}

/// A pool of $4,000 on ICP (and `base` on Base), the rebalancing's defaults, the ck minter
/// and the ckETH ledger configured, and one move opened.
fn pool_with_move(direction: MoveDirection, amount: TokenAmount, base: TokenAmount) -> u64 {
    crate::storage::init();
    funded_pool(usdc(4_000));
    if base > TokenAmount::ZERO {
        base_stake(
            "0x1111111111111111111111111111111111111111"
                .parse()
                .unwrap(),
            base,
            0x51,
            7,
        );
    }
    crate::storage::flow_settings::set(settlement_api::types::flow::FlowSettings {
        ck_minter: Some(minter()),
        cketh_ledger: Some(cketh()),
        cketh_fee_allowance: Nat::from(30_000_000_000_000_u64),
        ..Default::default()
    })
    .unwrap();
    record_at(
        PoolStep::MoveSettingsSet {
            settings: RebalanceSettings::default(),
        },
        at(1),
    )
    .unwrap();
    record_at(
        PoolStep::MoveOpened {
            id: 0,
            direction,
            amount,
            urgent: false,
            opened_at: at(2),
            window_ends: None,
            own_cost: TokenAmount::ZERO,
        },
        at(2),
    )
    .unwrap();
    0
}

#[test]
fn the_minter_is_approved_on_both_ledgers_with_the_moves_pair_then_asked() {
    on_fresh_memory(|| {
        let id = pool_with_move(MoveDirection::IcpToBase, usdc(1_000), TokenAmount::ZERO);
        let fake = Fake {
            balance: Nat::from(30_000_000_000_000_u64),
            ..Fake::default()
        };
        let item = super::super::item(id).unwrap();
        block_on(withdraw(&fake, &item, &ctx())).unwrap();
        let memo = move_ref(id).into_bytes();
        let nanos = ctx().now.as_nanos();
        let ckusdc = crate::storage::pool::tests::ckusdc();
        assert_eq!(
            fake.asked.borrow()[1..],
            [
                Asked::Approve(
                    ckusdc,
                    minter(),
                    Nat::from(usdc(1_000).checked_add(TokenAmount::from(FEE)).unwrap()),
                    memo,
                    nanos
                ),
                Asked::Approve(
                    cketh(),
                    minter(),
                    Nat::from(30_000_000_000_000_u64),
                    memo,
                    nanos
                ),
                Asked::Withdraw(
                    ckusdc,
                    Nat::from(usdc(1_000)),
                    "0x36D34038F04990a58c068e08eD9754e2E3D9bA53".to_string()
                ),
            ]
        );
        let item = super::super::item(id).unwrap();
        assert_eq!(item.status, MoveStatus::Withdrawn);
        assert_eq!(item.withdrawal, Some(42));
        let icp = ledger().pockets.icp;
        // in flight, the approval's fee the move's cost
        assert_eq!(icp.in_flight, usdc(1_000));
        assert_eq!(
            icp.free,
            usdc(3_000).checked_sub(TokenAmount::from(FEE)).unwrap()
        );
        assert_eq!(ledger().moves().cost, TokenAmount::from(FEE));
    });
}

#[test]
fn a_refusing_minter_puts_the_ckusdc_back_and_short_ckether_asks_nothing() {
    on_fresh_memory(|| {
        let id = pool_with_move(MoveDirection::IcpToBase, usdc(1_000), TokenAmount::ZERO);
        let short = Fake {
            balance: Nat::from(1_u8),
            ..Fake::default()
        };
        let item = super::super::item(id).unwrap();
        assert!(block_on(withdraw(&short, &item, &ctx())).is_err());
        assert_eq!(short.asked.borrow().len(), 1, "the balance read alone");
        assert_eq!(super::super::item(id).unwrap().status, MoveStatus::Open);
        let refusing = Fake {
            balance: Nat::from(30_000_000_000_000_u64),
            withdraw: Some(Err(CallError::Refused("no".to_string()))),
            ..Fake::default()
        };
        block_on(withdraw(&refusing, &item, &ctx())).unwrap();
        let item = super::super::item(id).unwrap();
        assert_eq!(item.status, MoveStatus::Open);
        let icp = ledger().pockets.icp;
        assert_eq!(
            (icp.reserved, icp.in_flight),
            (usdc(1_000), TokenAmount::ZERO)
        );
    });
}

/// Signs and burns move `id`'s pinned order as the outbox records it: the nonce handed out
/// for the move, its signed record, its close at depth.
fn sign_and_burn(id: u64) {
    use crate::storage::pool::tests::created;
    crate::storage::events::append_event_at(created(TxPurpose::PoolMove(id), 0), at(4)).unwrap();
    record_at(
        PoolStep::MoveSigned {
            id,
            chain_id: ChainId::BASE,
            nonce: types::Nonce::new(0),
            tx_hash: TxHash::new([0x71; 32]),
            raw_tx: vec![0x02, 0x72],
        },
        at(4),
    )
    .unwrap();
    record_at(
        PoolStep::MoveBurned {
            id,
            tx_hash: TxHash::new([0x71; 32]),
            block: BlockNumber::new(40),
            intent: None,
        },
        at(5),
    )
    .unwrap();
}

/// A Base to ICP move's own route burned: its sweep moves everything in the rebalance
/// subaccount, credits at most the move, and a lost answer sends the same transfer again.
#[test]
fn the_sweep_moves_all_of_the_subaccount_and_credits_the_move_once() {
    on_fresh_memory(|| {
        let id = pool_with_move(MoveDirection::BaseToIcp, usdc(1_000), usdc(6_000));
        record_at(
            PoolStep::MoveRouted {
                id,
                route: MoveRoute::Own {
                    max_fee: TokenAmount::ZERO,
                    fast: false,
                },
                order: PinnedOrder {
                    order_id: [1; 32],
                    order: vec![],
                    gas_limit: 300_000,
                    deadline_s: 1,
                },
                scan_from: None,
            },
            at(3),
        )
        .unwrap();
        sign_and_burn(id);
        assert_eq!(ledger().pockets.base.in_flight, usdc(1_000));
        let item = super::super::item(id).unwrap();
        // not all there: nothing but the read
        let early = Fake {
            balance: Nat::from(usdc(400)),
            ..Fake::default()
        };
        block_on(sweep(&early, &item, &ctx())).unwrap();
        assert_eq!(early.asked.borrow().len(), 1);
        assert_eq!(
            early.asked.borrow()[0],
            Asked::Balance(
                crate::storage::pool::tests::ckusdc(),
                Account::of(ctx().principal, Some(rebalance_subaccount()))
            )
        );
        // the mint and a $50 donation: all of it moves, the move is credited at most its
        // amount (standard finality: Circle kept nothing)
        let lost = Fake {
            balance: Nat::from(usdc(1_050)),
            transfer: Some(Err(CallError::Unknown("lost".to_string()))),
            ..Fake::default()
        };
        assert!(block_on(sweep(&lost, &item, &ctx())).is_err());
        let moved = usdc(1_050).checked_sub(TokenAmount::from(FEE)).unwrap();
        let sweeping = super::super::item(id).unwrap();
        assert_eq!(sweeping.status, MoveStatus::Sweeping);
        let recorded = sweeping.sweep.unwrap();
        assert_eq!((recorded.amount, recorded.credited), (moved, usdc(1_000)));
        // the same transfer again, with the pair it was recorded with
        let later = Ctx {
            now: at(200),
            ..ctx()
        };
        let answers = Fake::default();
        block_on(resweep(&answers, &sweeping, &later)).unwrap();
        let memo = move_ref(id).into_bytes();
        assert_eq!(
            answers.asked.borrow()[0],
            Asked::Transfer(
                Some(rebalance_subaccount()),
                Account::of(ctx().principal, None),
                Nat::from(moved),
                memo,
                ctx().now.as_nanos()
            )
        );
        assert_eq!(lost.asked.borrow()[1], answers.asked.borrow()[0]);
        let arrived = super::super::item(id).unwrap();
        assert_eq!(arrived.status, MoveStatus::Arrived);
        assert_eq!(arrived.arrived, Some(usdc(1_000)));
        let pockets = ledger().pockets;
        assert_eq!(pockets.base.in_flight, TokenAmount::ZERO);
        assert_eq!(pockets.icp.free, usdc(5_000));
        // credited once: a second sweep of the arrived move is refused by the fold
        assert!(block_on(resweep(&Fake::default(), &arrived, &later)).is_err());
    });
}

/// P11e: an ICP to Base move by Eco past its route's deadline is looked for on the Portals;
/// refunded unfilled it is routed again, filled with no report it stops after hours.
#[test]
fn an_eco_move_to_base_past_its_deadline_is_looked_for_on_the_portals() {
    use types::eco::{Fill, REWARD_REFUNDED, REWARD_WITHDRAWN};
    assert_eq!(unfilled_next(Fill::Open, REWARD_REFUNDED, false), Ok(true));
    assert_eq!(
        unfilled_next(Fill::Cancelled, REWARD_REFUNDED, false),
        Ok(true)
    );
    // not refunded yet: it waits (the rescue, or Eco's week)
    assert_eq!(unfilled_next(Fill::Open, 1, true), Ok(false));
    // filled: the arrival is the watcher's to report; after hours a controller looks
    assert_eq!(
        unfilled_next(Fill::Filled, REWARD_WITHDRAWN, false),
        Ok(false)
    );
    assert!(unfilled_next(Fill::Filled, REWARD_WITHDRAWN, true).is_err());

    // the Eco step of the canary's order, its route's deadline its own
    let order = include_str!("../../pay/samples/eco_base_to_arbitrum.hex");
    let order = hex::decode(order.trim()).unwrap();
    let deadline = eco_route_deadline(&order).unwrap();
    let mut item = item(MoveDirection::IcpToBase, MoveStatus::Burned);
    item.route = Some(MoveRoute::Proposed {
        fee: TokenAmount::ONE,
    });
    item.order = Some(PinnedOrder {
        order_id: [1; 32],
        order,
        gas_limit: 300_000,
        deadline_s: deadline,
    });
    let at_secs = |secs: u64| Timestamp::from_secs(secs).unwrap();
    assert_eq!(next(&item, at_secs(deadline), false), Next::Wait);
    assert_eq!(
        next(&item, at_secs(deadline + UNFILLED_MARGIN_S + 1), false),
        Next::Unfilled
    );
    // never stopped for hours of waiting: its USDC is somewhere the Portals tell
    assert_eq!(
        next(&item, at_secs(deadline + 10 * 3_600), false),
        Next::Unfilled
    );
}

/// P11e: a withdrawal the ck minter reimbursed returns its ckUSDC to the ICP pocket.
#[test]
fn a_reimbursed_withdrawal_returns_to_the_icp_pocket() {
    on_fresh_memory(|| {
        let id = pool_with_move(MoveDirection::IcpToBase, usdc(1_000), TokenAmount::ZERO);
        let item = super::super::item(id).unwrap();
        let fake = Fake {
            balance: Nat::from(30_000_000_000_000_u64),
            ..Fake::default()
        };
        block_on(withdraw(&fake, &item, &ctx())).unwrap();
        let withdrawn = super::super::item(id).unwrap();
        let reimbursed = Fake {
            status: Some(ic::WithdrawalStatus::TxFinalized(
                ic::TxFinalizedStatus::Reimbursed {
                    transaction_hash: "0x01".to_string(),
                    reimbursed_amount: Nat::from(usdc(1_000)),
                    reimbursed_in_block: Nat::from(55_u8),
                },
            )),
            ..Fake::default()
        };
        block_on(landing(&reimbursed, &withdrawn, &ctx())).unwrap();
        let returned = super::super::item(id).unwrap();
        assert!(matches!(returned.status, MoveStatus::Returned(_)));
        let icp = ledger().pockets.icp;
        assert_eq!(icp.in_flight, TokenAmount::ZERO);
        // all of it back but the approval's fee, the move's cost
        assert_eq!(
            icp.free,
            usdc(4_000).checked_sub(TokenAmount::from(FEE)).unwrap()
        );
    });
}

/// CR-07: a proposal whose order would be past its signing margin by the time the move is
/// signed is passed over for the canister's own route: taken, it would only stop the move as
/// stuck at its first signature.
#[test]
fn a_proposal_too_close_to_its_deadline_is_passed_over() {
    use types::pool::{PinnedOrder, Proposal};
    let open = item(MoveDirection::BaseToIcp, MoveStatus::Open);
    let now = at(60);
    let proposed = |deadline_s: u64| Move {
        proposal: Some(Proposal {
            order: PinnedOrder {
                order_id: [1; 32],
                order: vec![1],
                gas_limit: 300_000,
                deadline_s,
            },
            fee: TokenAmount::from(100_000_u64),
        }),
        ..open.clone()
    };
    let in_time = proposed(now.as_secs().get() + SIGN_MARGIN_S + 1);
    assert_eq!(proposal_to_take(&in_time, now), in_time.proposal.as_ref());
    let too_late = proposed(now.as_secs().get() + SIGN_MARGIN_S);
    assert_eq!(proposal_to_take(&too_late, now), None);
    // an urgent move never takes a proposal
    let urgent = Move {
        urgent: true,
        ..in_time.clone()
    };
    assert_eq!(proposal_to_take(&urgent, now), None);
}
