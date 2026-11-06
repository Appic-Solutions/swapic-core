//! The driver's steps (P11c Task 4): what a move does next, and the ICP legs on a fake ledger
//! and minter: the minter's call with its approvals, its landing, and the sweep, credited at
//! most the move's amount (a donation is never the pool's).

use super::*;
use crate::storage::on_fresh_memory;
use crate::storage::pool::tests::{base_stake, funded_pool, usdc, FEE};
use crate::storage::pool::{ledger, record_at};
use crate::test_util::block_on;
use std::cell::RefCell;
use types::events::TxPurpose;
use types::pool::RebalanceSettings;

/// The wait past an Eco route's deadline before its fill is read, where a test does not look
/// at it.
const FILL_WAIT: std::time::Duration = std::time::Duration::from_secs(300);

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
    assert_eq!(next(&open, at(30), false, FILL_WAIT), Next::Wait);
    assert_eq!(next(&open, at(60), false, FILL_WAIT), Next::Route);
    let urgent = Move {
        urgent: true,
        window_ends: None,
        ..open.clone()
    };
    assert_eq!(next(&urgent, at(0), false, FILL_WAIT), Next::Route);
    // stuck, final or with a transaction out: nothing
    let stuck = Move {
        stuck: Some("a test".to_string()),
        ..open.clone()
    };
    assert_eq!(next(&stuck, at(90), false, FILL_WAIT), Next::Wait);
    assert_eq!(next(&open, at(90), true, FILL_WAIT), Next::Wait);
}

#[test]
fn a_move_to_base_asks_the_minter_then_reads_its_landing_then_takes_its_window() {
    let open = Move {
        window_ends: None,
        ..item(MoveDirection::IcpToBase, MoveStatus::Open)
    };
    assert_eq!(next(&open, at(0), false, FILL_WAIT), Next::Withdraw);
    // a minter's call whose answer was lost: a controller reads the minter
    let withdrawing = Move {
        status: MoveStatus::Withdrawing,
        ..open.clone()
    };
    assert!(matches!(
        next(&withdrawing, at(0), false, FILL_WAIT),
        Next::Stuck(_)
    ));
    let withdrawn = Move {
        status: MoveStatus::Withdrawn,
        ..open.clone()
    };
    assert_eq!(next(&withdrawn, at(600), false, FILL_WAIT), Next::Landing);
    // past its time it is still read: the landing stops it, and hears the minter after
    assert_eq!(
        next(&withdrawn, at(3 * 3_600), false, FILL_WAIT),
        Next::Landing
    );
    let stopped = Move {
        stuck: Some("a test".to_string()),
        ..withdrawn
    };
    assert_eq!(
        next(&stopped, at(4 * 3_600), false, FILL_WAIT),
        Next::Landing
    );
    assert_eq!(next(&stopped, at(4 * 3_600), true, FILL_WAIT), Next::Wait);
    let landed = Move {
        status: MoveStatus::Landed,
        window_ends: Some(at(1_260)),
        since: at(1_200),
        ..open
    };
    assert_eq!(next(&landed, at(1_230), false, FILL_WAIT), Next::Wait);
    assert_eq!(next(&landed, at(1_260), false, FILL_WAIT), Next::Route);
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
    assert_eq!(
        next(&routed, at(0), false, FILL_WAIT),
        Next::Sign { again: false }
    );
    routed.tx_hash = Some(TxHash::new([2; 32]));
    routed.reverts = Some(1);
    // the last review (item 2): a revert that carries no block can never be read at or after
    // it, so the move is held for a human at once, never looped to its deadline
    assert!(matches!(
        next(&routed, at(0), false, FILL_WAIT),
        Next::Stuck(why) if why.contains("no recorded block")
    ));
    routed.reverted_block = Some(BlockNumber::new(500));
    assert_eq!(
        next(&routed, at(0), false, FILL_WAIT),
        Next::Sign { again: true }
    );
    routed.reverts = Some(MAX_REVERTS);
    assert!(matches!(
        next(&routed, at(0), false, FILL_WAIT),
        Next::Stuck(_)
    ));
    routed.reverts = Some(1);
    assert!(matches!(
        next(&routed, at(3_500), false, FILL_WAIT),
        Next::Stuck(_)
    ));
}

#[test]
fn a_burned_move_to_icp_sweeps_or_looks_for_its_eco_fill() {
    let mut own = item(MoveDirection::BaseToIcp, MoveStatus::Burned);
    own.route = Some(MoveRoute::Own {
        max_fee: TokenAmount::ZERO,
        fast: false,
    });
    assert_eq!(next(&own, at(600), false, FILL_WAIT), Next::Sweep);
    assert!(matches!(
        next(&own, at(3 * 3_600), false, FILL_WAIT),
        Next::Stuck(_)
    ));
    let mut eco = own.clone();
    eco.route = Some(MoveRoute::Proposed {
        fee: TokenAmount::from(100_u64),
    });
    assert_eq!(next(&eco, at(60), false, FILL_WAIT), Next::Fill);
    eco.landed = Some(types::pool::Landing {
        tx: TxHash::new([3; 32]),
        block: BlockNumber::new(1),
        amount: usdc(499),
    });
    assert_eq!(next(&eco, at(60), false, FILL_WAIT), Next::Sweep);
    let landed = Move {
        status: MoveStatus::Landed,
        ..eco.clone()
    };
    assert_eq!(next(&landed, at(60), false, FILL_WAIT), Next::Deposit);
    let sweeping = Move {
        status: MoveStatus::Sweeping,
        ..eco
    };
    assert_eq!(next(&sweeping, at(60), false, FILL_WAIT), Next::Resweep);
    // a move to Base waits on Base for the watcher's report, and stops after hours
    let up = item(MoveDirection::IcpToBase, MoveStatus::Burned);
    assert_eq!(next(&up, at(600), false, FILL_WAIT), Next::Wait);
    assert!(matches!(
        next(&up, at(3 * 3_600), false, FILL_WAIT),
        Next::Stuck(_)
    ));
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
    Landing(TxHash),
}

#[derive(Default)]
struct Fake {
    asked: RefCell<Vec<Asked>>,
    balance: Nat,
    withdraw: Option<Result<u64, ic::WithdrawFailure>>,
    transfer: Option<Result<u64, CallError>>,
    status: Option<ic::WithdrawalStatus>,
    /// The minter's status read answers this instead.
    status_fails: Option<CallError>,
    /// What the landing read answers.
    landed: Option<Result<TransferRead, String>>,
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
    ) -> Result<ic::RetrieveErc20Request, ic::WithdrawFailure> {
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
        if let Some(error) = &self.status_fails {
            return Err(error.clone());
        }
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

    async fn landing(&self, tx: TxHash, _canister: EvmAddress) -> Result<TransferRead, String> {
        self.asked.borrow_mut().push(Asked::Landing(tx));
        self.landed.clone().unwrap_or(Ok(TransferRead::NotYet))
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
            withdraw: Some(Err(ic::WithdrawFailure::Refused(
                ic::WithdrawErc20Error::TemporarilyUnavailable("no".to_string()),
            ))),
            ..Fake::default()
        };
        block_on(withdraw(&refusing, &item, &ctx())).unwrap();
        let item = super::super::item(id).unwrap();
        assert_eq!(item.status, MoveStatus::Open);
        assert_eq!(item.stuck, None, "a passing refusal is asked again");
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
    // the last review (item 3): looked for once a block every node agrees on can be past
    // the deadline on Base, the payouts' own rule (`agreed::fill_wait`)
    let wait = crate::agreed::fill_wait(ChainId::BASE, 1);
    assert_eq!(next(&item, at_secs(deadline), false, wait), Next::Wait);
    assert_eq!(
        next(&item, at_secs(deadline + wait.as_secs()), false, wait),
        Next::Wait
    );
    assert_eq!(
        next(&item, at_secs(deadline + wait.as_secs() + 1), false, wait),
        Next::Unfilled
    );
    // never stopped for hours of waiting: its USDC is somewhere the Portals tell
    assert_eq!(
        next(&item, at_secs(deadline + 10 * 3_600), false, wait),
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

const WITHDRAWAL_TX: TxHash = TxHash::new([0x44; 32]);

fn sent() -> ic::WithdrawalStatus {
    ic::WithdrawalStatus::TxSent(ic::EthTransaction {
        transaction_hash: format!("0x{}", hex::encode(WITHDRAWAL_TX.as_ref())),
    })
}

fn succeeded() -> ic::WithdrawalStatus {
    ic::WithdrawalStatus::TxFinalized(ic::TxFinalizedStatus::Success {
        transaction_hash: format!("0x{}", hex::encode(WITHDRAWAL_TX.as_ref())),
        effective_transaction_fee: None,
    })
}

fn reimbursed(amount: TokenAmount) -> ic::WithdrawalStatus {
    ic::WithdrawalStatus::TxFinalized(ic::TxFinalizedStatus::Reimbursed {
        transaction_hash: format!("0x{}", hex::encode(WITHDRAWAL_TX.as_ref())),
        reimbursed_amount: Nat::from(amount),
        reimbursed_in_block: Nat::from(55_u8),
    })
}

fn landed(amount: TokenAmount) -> TransferRead {
    TransferRead::Landed {
        amount,
        block: BlockNumber::new(90),
    }
}

/// A move of $1,000 at `Withdrawn`, its minter's call answered.
fn withdrawn_move() -> u64 {
    let id = pool_with_move(MoveDirection::IcpToBase, usdc(1_000), TokenAmount::ZERO);
    let item = super::super::item(id).unwrap();
    let fake = Fake {
        balance: Nat::from(30_000_000_000_000_u64),
        ..Fake::default()
    };
    block_on(withdraw(&fake, &item, &ctx())).unwrap();
    id
}

fn context_at(secs: u64) -> Ctx {
    Ctx {
        now: at(secs),
        ..ctx()
    }
}

/// A clock past the move's wait, from its `since` at `ctx().now`.
const PAST_WAIT: u64 = 100 + 3 * 3_600;

#[test]
fn only_a_transaction_that_moved_the_whole_amount_lands() {
    let amount = usdc(1_000);
    let (hash, tx) = (WITHDRAWAL_TX, MinterSays::Sent(WITHDRAWAL_TX));
    let block = BlockNumber::new(90);
    let land = LandingStep::Land {
        tx: hash,
        amount,
        block,
    };
    // mined, at depth, successful, all of it at the canister: landed, with or without the
    // minter's finalized word, and however long it took
    for waited in [false, true] {
        assert_eq!(
            landing_step(&tx, Some(landed(amount)), amount, waited),
            land
        );
        assert_eq!(
            landing_step(
                &MinterSays::Succeeded(hash),
                Some(landed(amount)),
                amount,
                waited
            ),
            land
        );
    }
    let stops = |says: &MinterSays, read: Option<TransferRead>| {
        assert_eq!(
            landing_step(says, read, amount, false),
            LandingStep::Wait,
            "it waits at first"
        );
        assert!(matches!(
            landing_step(says, read, amount, true),
            LandingStep::Stop(_)
        ));
    };
    let short = amount.checked_sub(TokenAmount::ONE).unwrap();
    stops(&tx, Some(TransferRead::NotYet));
    stops(&tx, Some(TransferRead::Reverted));
    stops(&tx, Some(TransferRead::NoTransfer));
    stops(&tx, Some(landed(short)));
    stops(&MinterSays::Succeeded(hash), Some(TransferRead::Reverted));
    stops(&MinterSays::Succeeded(hash), Some(TransferRead::NoTransfer));
    stops(&MinterSays::Nothing, None);
    // the reimbursement is the minter's final word, whatever else was read
    let reimbursed = MinterSays::Reimbursed { amount, block: 55 };
    assert_eq!(
        landing_step(&reimbursed, Some(TransferRead::Reverted), amount, true),
        LandingStep::Reimburse { amount, block: 55 }
    );
}

/// The fix: a transaction the minter still calls `TxSent` lands once it is mined at depth,
/// long before `TxFinalized`, and such a move goes on to Base as any other: routed, burned and
/// credited there, nothing left in flight.
#[test]
fn a_sent_and_mined_withdrawal_lands_and_arrives_on_base() {
    on_fresh_memory(|| {
        let id = withdrawn_move();
        let item = super::super::item(id).unwrap();
        let unmined = Fake {
            status: Some(sent()),
            ..Fake::default()
        };
        block_on(landing(&unmined, &item, &context_at(160))).unwrap();
        let item = super::super::item(id).unwrap();
        assert_eq!(item.status, MoveStatus::Withdrawn);
        let mined = Fake {
            status: Some(sent()),
            landed: Some(Ok(landed(usdc(1_000)))),
            ..Fake::default()
        };
        block_on(landing(&mined, &item, &context_at(220))).unwrap();
        assert_eq!(
            mined.asked.borrow().as_slice(),
            &[Asked::Landing(WITHDRAWAL_TX)]
        );
        let item = super::super::item(id).unwrap();
        assert_eq!(item.status, MoveStatus::Landed);
        assert_eq!(item.landed.map(|landing| landing.tx), Some(WITHDRAWAL_TX));
        assert_eq!(item.stuck, None);
        // on from there as ever: its window, its route, its burn, its arrival on Base
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
                    deadline_s: at(3_600).as_secs().get(),
                },
                scan_from: None,
            },
            at(230),
        )
        .unwrap();
        sign_and_burn(id);
        let credit = types::pool::CreditKey {
            chain: ChainId::BASE,
            tx: TxHash::new([0x72; 32]),
            log_index: 3,
        };
        record_at(
            PoolStep::MoveArrived {
                id,
                credited: usdc(1_000),
                credit: Some(credit),
                block: Some(BlockNumber::new(50)),
                ledger_block: None,
            },
            at(300),
        )
        .unwrap();
        let pockets = ledger().pockets;
        assert_eq!(pockets.icp.in_flight, TokenAmount::ZERO);
        assert_eq!(pockets.base.free, usdc(1_000));
        assert_eq!(super::super::item(id).unwrap().status, MoveStatus::Arrived);
    });
}

/// A failed withdrawal is never a landing, and its reimbursement is booked whenever it comes:
/// the real one took the transaction's finalized word, which can lag by days. The stop is an
/// alert, not the end: no controller is needed for the money to come back.
#[test]
fn a_reverted_withdrawal_stops_and_its_late_reimbursement_is_still_booked() {
    on_fresh_memory(|| {
        let id = withdrawn_move();
        let reverted = Fake {
            status: Some(sent()),
            landed: Some(Ok(TransferRead::Reverted)),
            ..Fake::default()
        };
        let item = super::super::item(id).unwrap();
        block_on(landing(&reverted, &item, &context_at(600))).unwrap();
        let item = super::super::item(id).unwrap();
        assert_eq!(
            (item.status.clone(), item.stuck.clone()),
            (MoveStatus::Withdrawn, None)
        );
        block_on(landing(&reverted, &item, &context_at(PAST_WAIT))).unwrap();
        let stopped = super::super::item(id).unwrap();
        assert!(stopped.stuck.as_deref().unwrap().contains("reverted"));
        assert_eq!(stopped.status, MoveStatus::Withdrawn);
        assert_eq!(ledger().pockets.icp.in_flight, usdc(1_000));
        // still `TxSent`, days on: it is still read, never lands, and is not stopped twice
        assert_eq!(
            next(&stopped, at(PAST_WAIT), false, FILL_WAIT),
            Next::Landing
        );
        block_on(landing(&reverted, &stopped, &context_at(PAST_WAIT + 3_600))).unwrap();
        let same = super::super::item(id).unwrap();
        assert_eq!(same.status, MoveStatus::Withdrawn);
        assert_eq!(same.stuck, stopped.stuck);
        // the minter reimburses, 7 days on
        let paid = Fake {
            status: Some(reimbursed(usdc(1_000))),
            ..Fake::default()
        };
        let days = 7 * 24 * 3_600;
        block_on(landing(&paid, &stopped, &context_at(days))).unwrap();
        let returned = super::super::item(id).unwrap();
        assert!(matches!(returned.status, MoveStatus::Returned(_)));
        assert_eq!(returned.stuck, None);
        let icp = ledger().pockets.icp;
        assert_eq!(icp.in_flight, TokenAmount::ZERO);
        assert_eq!(
            icp.free,
            usdc(4_000).checked_sub(TokenAmount::from(FEE)).unwrap()
        );
    });
}

/// A transaction mined past the stop (the worst of 885 took 181.7 minutes, then its depth) lands
/// when it is read, with the minter still at `TxSent`: not at its `TxFinalized`, 56.9 hours at the
/// p90, and with no controller.
#[test]
fn a_stopped_withdrawal_lands_when_its_transaction_is_mined() {
    on_fresh_memory(|| {
        let id = withdrawn_move();
        let unmined = Fake {
            status: Some(sent()),
            ..Fake::default()
        };
        let item = super::super::item(id).unwrap();
        block_on(landing(&unmined, &item, &context_at(PAST_WAIT))).unwrap();
        let stopped = super::super::item(id).unwrap();
        assert!(stopped.stuck.is_some());
        let mined = Fake {
            status: Some(sent()),
            landed: Some(Ok(landed(usdc(1_000)))),
            ..Fake::default()
        };
        block_on(landing(&mined, &stopped, &context_at(PAST_WAIT + 600))).unwrap();
        let item = super::super::item(id).unwrap();
        assert_eq!(item.status, MoveStatus::Landed);
        assert_eq!(item.stuck, None);
        assert!(item.routable());
    });
}

/// The minter's latest hash may not be the one mined (it replaces a transaction with a dearer
/// one): nothing lands on it, and its finalized word names the mined one, after a stop.
#[test]
fn a_stopped_withdrawal_lands_when_the_minter_finalizes_it() {
    on_fresh_memory(|| {
        let id = withdrawn_move();
        let replaced = Fake {
            status: Some(sent()),
            ..Fake::default()
        };
        let item = super::super::item(id).unwrap();
        block_on(landing(&replaced, &item, &context_at(PAST_WAIT))).unwrap();
        let stopped = super::super::item(id).unwrap();
        assert!(stopped.stuck.is_some());
        let finalized = Fake {
            status: Some(succeeded()),
            landed: Some(Ok(landed(usdc(1_000)))),
            ..Fake::default()
        };
        block_on(landing(&finalized, &stopped, &context_at(PAST_WAIT + 600))).unwrap();
        let item = super::super::item(id).unwrap();
        assert_eq!(item.status, MoveStatus::Landed);
        assert_eq!(item.stuck, None, "it goes on with no controller");
        assert!(item.routable());
    });
}

/// A wait is a wait whichever way it fails: a minter that does not answer, or does not know
/// the withdrawal, still stops the move at its time, as `next` once did on its own, and a
/// stopped move is never stopped twice.
#[test]
fn a_landing_read_that_keeps_failing_still_stops_the_move() {
    for answers in [false, true] {
        on_fresh_memory(move || {
            let id = withdrawn_move();
            let fake = Fake {
                status_fails: (!answers).then(|| CallError::Unknown("no answer".to_string())),
                ..Fake::default()
            };
            let item = super::super::item(id).unwrap();
            assert!(block_on(landing(&fake, &item, &context_at(600))).is_err());
            assert_eq!(super::super::item(id).unwrap().stuck, None);
            block_on(landing(&fake, &item, &context_at(PAST_WAIT))).unwrap();
            let stopped = super::super::item(id).unwrap();
            assert!(stopped.stuck.is_some());
            block_on(landing(&fake, &stopped, &context_at(PAST_WAIT + 600))).unwrap();
        });
    }
}

fn ledger_error() -> ic::LedgerError {
    ic::LedgerError::InsufficientFunds {
        balance: Nat::from(1_u8),
        failed_burn_amount: Nat::from(2_u8),
        token_symbol: "ckUSDC".to_string(),
        ledger_id: Principal::from_slice(&[0x60; 10]),
    }
}

/// What a typed refusal does to a move, ckUSDC never burned in any of them: back to wait for
/// the next tick, and the ones that cannot pass held for a controller, not asked again each
/// minute at a ledger fee.
#[test]
fn a_refusal_that_cannot_pass_stops_and_one_that_can_is_asked_again() {
    use ic::{LedgerError, WithdrawErc20Error as Refusal};
    let busy = || LedgerError::TemporarilyUnavailable("busy".to_string());
    let cases = [
        (Refusal::TemporarilyUnavailable("gas".to_string()), false),
        (
            Refusal::CkEthLedgerError {
                error: ledger_error(),
            },
            false,
        ),
        (
            Refusal::CkErc20LedgerError {
                cketh_block_index: Nat::from(9_u8),
                error: busy(),
            },
            false,
        ),
        (
            Refusal::CkErc20LedgerError {
                cketh_block_index: Nat::from(9_u8),
                error: ledger_error(),
            },
            true,
        ),
        (
            Refusal::RecipientAddressBlocked {
                address: "0x01".to_string(),
            },
            true,
        ),
        (
            Refusal::TokenNotSupported {
                supported_tokens: vec![],
            },
            true,
        ),
    ];
    for (refusal, stops) in cases {
        on_fresh_memory(move || {
            let id = pool_with_move(MoveDirection::IcpToBase, usdc(1_000), TokenAmount::ZERO);
            let item = super::super::item(id).unwrap();
            let fake = Fake {
                balance: Nat::from(30_000_000_000_000_u64),
                withdraw: Some(Err(ic::WithdrawFailure::Refused(refusal.clone()))),
                ..Fake::default()
            };
            block_on(withdraw(&fake, &item, &ctx())).unwrap();
            let item = super::super::item(id).unwrap();
            assert_eq!(item.status, MoveStatus::Open, "{refusal:?}");
            assert_eq!(item.stuck.is_some(), stops, "{refusal:?}");
            assert!(!item.left, "{refusal:?}: nothing left the pocket");
            let icp = ledger().pockets.icp;
            assert_eq!(
                (icp.reserved, icp.in_flight),
                (usdc(1_000), TokenAmount::ZERO)
            );
            let expected = if stops { Next::Wait } else { Next::Withdraw };
            assert_eq!(next(&item, at(200), false, FILL_WAIT), expected);
        });
    }
}

/// A trap of the minter's (its guards answer that way) or a lost reply is no refusal: the
/// ckUSDC may be burned, so nothing is reserved again and nothing is asked again. The move
/// holds its line in flight and stops for a controller at the next tick.
#[test]
fn a_lost_answer_is_never_read_as_nothing_burned() {
    on_fresh_memory(|| {
        let id = pool_with_move(MoveDirection::IcpToBase, usdc(1_000), TokenAmount::ZERO);
        let item = super::super::item(id).unwrap();
        let trapped = Fake {
            balance: Nat::from(30_000_000_000_000_u64),
            withdraw: Some(Err(ic::WithdrawFailure::Unknown(
                "Failed retrieving guard for principal".to_string(),
            ))),
            ..Fake::default()
        };
        assert!(block_on(withdraw(&trapped, &item, &ctx())).is_err());
        let item = super::super::item(id).unwrap();
        assert_eq!(item.status, MoveStatus::Withdrawing);
        assert!(item.left);
        let icp = ledger().pockets.icp;
        assert_eq!(
            (icp.reserved, icp.in_flight),
            (TokenAmount::ZERO, usdc(1_000))
        );
        assert!(matches!(
            next(&item, at(200), false, FILL_WAIT),
            Next::Stuck(why) if why.contains("lost")
        ));
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

/// Review of the merged canister (item 6): the Portals' last reads of moves no longer open
/// are dropped, so the remembered reads never grow past the open moves.
#[test]
fn the_unfilled_reads_of_closed_moves_are_dropped() {
    on_fresh_memory(|| {
        crate::storage::init();
        crate::storage::timer_state::update(|state| {
            state.unfilled_reads.insert(4_242, at(0));
            ((), true)
        });
        assert!(late_read_due(7, at(1)));
        let reads = crate::storage::timer_state::get().unfilled_reads;
        assert!(!reads.contains_key(&4_242), "no move 4242 is open");
    });
}
