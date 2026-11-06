//! Each move step's pocket lines (P11c Task 1), and the rules the fold holds a move to
//! (Review Focus 2 and 4 from the fold's side).

use super::super::tests::{settings, usdc, Pool};
use super::*;
use crate::chain::ChainId;
use crate::hash::TxHash;
use crate::hash::{OwnerKey, PoolRef};
use crate::numeric::{BlockNumber, Nonce};
use crate::pool::payout::PinnedOrder;
use crate::pool::RebalanceSettings;
use crate::pool::{CreditKey, EvmPayout, NonceUse, Registered, Shares};
use std::collections::BTreeMap;

/// The pool's maps on the heap and the moves beside them, steps applied as the storage
/// layer applies them: `admits`, then every effect, each line at its own index and instant.
struct World {
    pool: Pool,
    moves: BTreeMap<u64, Move>,
    index: u64,
    now: u64,
}

impl PoolLookup for World {
    fn deposit(&self, r#ref: &PoolRef) -> Option<Registered> {
        self.pool.deposit(r#ref)
    }

    fn shares_of(&self, owner: &OwnerKey) -> Shares {
        self.pool.shares_of(owner)
    }

    fn is_credited(&self, key: &CreditKey) -> bool {
        self.pool.is_credited(key)
    }

    fn payout(&self, id: u64) -> Option<EvmPayout> {
        self.pool.payout(id)
    }

    fn queue_head(&self) -> Option<(u64, u64)> {
        self.pool.queue_head()
    }

    fn nonce_used(&self, key: &NonceUse) -> bool {
        self.pool.nonce_used(key)
    }

    fn orphan(&self, key: &CreditKey) -> Option<crate::pool::Orphan> {
        self.pool.orphan(key)
    }

    fn pool_move(&self, id: u64) -> Option<Move> {
        self.moves.get(&id).cloned()
    }
}

impl World {
    /// A pool with settings, $6,000 free on Base and $4,000 on ICP, and the rebalancing's
    /// defaults.
    fn new() -> Self {
        let mut world = Self {
            pool: Pool::default(),
            moves: BTreeMap::new(),
            index: 100,
            now: 1_800_000_000,
        };
        world
            .step(PoolStep::SettingsSet {
                settings: settings(),
            })
            .unwrap();
        world
            .step(PoolStep::MoveSettingsSet {
                settings: RebalanceSettings::default(),
            })
            .unwrap();
        world.pool.ledger.pockets.base.free = usdc(6_000);
        world.pool.ledger.pockets.icp.free = usdc(4_000);
        world
    }

    fn at(&self) -> Timestamp {
        Timestamp::from_secs(self.now).unwrap()
    }

    fn step(&mut self, step: PoolStep) -> Result<(), PoolStepError> {
        self.pool.ledger.admits(&step, self)?;
        let effects = self
            .pool
            .ledger
            .clone()
            .apply_at(&step, self, self.at(), self.index);
        self.index += 1;
        self.pool.ledger = effects.ledger;
        if let Some(key) = effects.credited {
            self.pool.credited.insert(key);
        }
        if let Some(item) = effects.moved {
            self.moves.insert(item.id, item);
        }
        Ok(())
    }

    fn ledger(&self) -> &PoolLedger {
        &self.pool.ledger
    }

    fn item(&self, id: u64) -> Move {
        self.moves[&id].clone()
    }

    fn open(&mut self, direction: MoveDirection, amount: TokenAmount, urgent: bool) -> u64 {
        let id = self.ledger().moves().next_id;
        self.step(opened(id, direction, amount, urgent, self.at()))
            .unwrap();
        id
    }
}

fn opened(
    id: u64,
    direction: MoveDirection,
    amount: TokenAmount,
    urgent: bool,
    at: Timestamp,
) -> PoolStep {
    PoolStep::MoveOpened {
        id,
        direction,
        amount,
        urgent,
        opened_at: at,
        window_ends: None,
        own_cost: TokenAmount::from(90_000_u64),
    }
}

fn order(byte: u8) -> PinnedOrder {
    PinnedOrder {
        order_id: [byte; 32],
        order: vec![byte; 4],
        gas_limit: 300_000,
        deadline_s: 1_800_003_600,
    }
}

fn own() -> MoveRoute {
    MoveRoute::Own {
        max_fee: TokenAmount::ZERO,
        fast: false,
    }
}

fn signed(id: u64, chain_id: ChainId) -> PoolStep {
    PoolStep::MoveSigned {
        id,
        chain_id,
        nonce: Nonce::new(5),
        tx_hash: TxHash::new([0x51; 32]),
        raw_tx: vec![0x02, 0x52],
    }
}

fn burned(id: u64, intent: Option<[u8; 32]>) -> PoolStep {
    PoolStep::MoveBurned {
        id,
        tx_hash: TxHash::new([0x51; 32]),
        block: BlockNumber::new(40),
        intent,
    }
}

fn key(byte: u8) -> CreditKey {
    CreditKey {
        chain: ChainId::BASE,
        tx: TxHash::new([byte; 32]),
        log_index: 3,
    }
}

#[test]
fn a_base_to_icp_move_reserves_leaves_and_arrives_on_its_own_route() {
    let mut world = World::new();
    let id = world.open(MoveDirection::BaseToIcp, usdc(1_000), false);
    let base = world.ledger().pockets.base;
    assert_eq!((base.free, base.reserved), (usdc(5_000), usdc(1_000)));
    assert_eq!(world.item(id).status, MoveStatus::Open);

    world
        .step(PoolStep::MoveRouted {
            id,
            route: own(),
            order: order(1),
            scan_from: None,
        })
        .unwrap();
    // signing leaves the Base pocket: in flight, expected at the address no more
    world.step(signed(id, ChainId::BASE)).unwrap();
    let base = world.ledger().pockets.base;
    assert_eq!(
        (base.reserved, base.in_flight),
        (TokenAmount::ZERO, usdc(1_000))
    );
    assert!(matches!(world.item(id).status, MoveStatus::Signed(_)));
    // the assets keep it: in flight stays the stakers'
    assert_eq!(world.ledger().total_assets(), usdc(10_000));

    world.step(burned(id, None)).unwrap();
    // the ck minter minted 1,000 less nothing; the sweep's fee is the move's cost
    let swept = TokenAmount::from(999_990_000_u64);
    world
        .step(PoolStep::MoveSweeping {
            id,
            created_at: world.at(),
            amount: swept,
            credited: swept,
        })
        .unwrap();
    world
        .step(PoolStep::MoveArrived {
            id,
            credited: swept,
            credit: None,
            block: None,
            ledger_block: Some(77),
        })
        .unwrap();
    let pockets = world.ledger().pockets;
    assert_eq!(pockets.base.in_flight, TokenAmount::ZERO);
    assert_eq!(pockets.icp.free, usdc(4_000).checked_add(swept).unwrap());
    let lines = world.ledger().moves();
    assert_eq!(lines.cost, TokenAmount::from(10_000_u64));
    assert!(lines.open.is_empty());
    assert_eq!(world.item(id).arrived, Some(swept));
}

#[test]
fn an_icp_to_base_move_leaves_at_the_minter_and_arrives_once_on_base() {
    let mut world = World::new();
    let id = world.open(MoveDirection::IcpToBase, usdc(2_000), false);
    assert_eq!(world.ledger().pockets.icp.reserved, usdc(2_000));
    // the minter's call: in flight before it is asked
    world
        .step(PoolStep::MoveWithdrawing {
            id,
            created_at: world.at(),
        })
        .unwrap();
    let icp = world.ledger().pockets.icp;
    assert_eq!(
        (icp.reserved, icp.in_flight),
        (TokenAmount::ZERO, usdc(2_000))
    );
    // the approval's ckUSDC fee is the pool's cost
    let fee = TokenAmount::from(10_000_u64);
    world
        .step(PoolStep::MoveWithdrawn {
            id,
            withdrawal_id: 42,
            fee,
        })
        .unwrap();
    assert_eq!(
        world.ledger().pockets.icp.free,
        usdc(2_000).checked_sub(fee).unwrap()
    );
    world.now += 1_200;
    let window = Timestamp::from_secs(world.now + 60).unwrap();
    world
        .step(PoolStep::MoveLanded {
            id,
            tx_hash: TxHash::new([0x61; 32]),
            block: BlockNumber::new(23_000_000),
            amount: usdc(2_000),
            window_ends: Some(window),
        })
        .unwrap();
    let landed = world.item(id);
    assert_eq!(landed.window_ends, Some(window));
    assert!(landed.proposable());
    world
        .step(PoolStep::MoveRouted {
            id,
            route: own(),
            order: order(2),
            scan_from: None,
        })
        .unwrap();
    world.step(signed(id, ChainId::ETHEREUM)).unwrap();
    // the Ethereum order moves no pocket line: the ckUSDC left ICP already
    assert_eq!(world.ledger().pockets.icp.in_flight, usdc(2_000));
    world.step(burned(id, None)).unwrap();
    let arrived = PoolStep::MoveArrived {
        id,
        credited: usdc(2_000),
        credit: Some(key(0x71)),
        block: Some(BlockNumber::new(37_000_000)),
        ledger_block: None,
    };
    world.step(arrived.clone()).unwrap();
    let pockets = world.ledger().pockets;
    assert_eq!(pockets.icp.in_flight, TokenAmount::ZERO);
    assert_eq!(pockets.base.free, usdc(8_000));
    assert!(world.pool.credited.contains(&key(0x71)));
    // the arrival is in the lines and not yet in a balance read at a lower block
    assert_eq!(
        world
            .ledger()
            .arrivals_above(ChainId::BASE, BlockNumber::new(36_999_999)),
        usdc(2_000)
    );
    // credited once (Review Focus 2): the same report again is refused
    assert!(world.step(arrived).is_err());
    assert_eq!(world.ledger().moves().cost, fee);
}

#[test]
fn a_transfer_credited_already_never_credits_a_move() {
    let mut world = World::new();
    let id = world.open(MoveDirection::IcpToBase, usdc(500), false);
    for step in [
        PoolStep::MoveWithdrawing {
            id,
            created_at: world.at(),
        },
        PoolStep::MoveWithdrawn {
            id,
            withdrawal_id: 1,
            fee: TokenAmount::ZERO,
        },
        PoolStep::MoveLanded {
            id,
            tx_hash: TxHash::new([0x61; 32]),
            block: BlockNumber::new(1),
            amount: usdc(500),
            window_ends: None,
        },
        PoolStep::MoveRouted {
            id,
            route: own(),
            order: order(2),
            scan_from: None,
        },
        signed(id, ChainId::ETHEREUM),
        burned(id, None),
    ] {
        world.step(step).unwrap();
    }
    world.pool.credited.insert(key(0x72));
    assert_eq!(
        world.step(PoolStep::MoveArrived {
            id,
            credited: usdc(500),
            credit: Some(key(0x72)),
            block: Some(BlockNumber::new(2)),
            ledger_block: None,
        }),
        Err(PoolStepError::AlreadyCredited(key(0x72)))
    );
    // nor more than the move's amount
    assert!(world
        .step(PoolStep::MoveArrived {
            id,
            credited: usdc(501),
            credit: Some(key(0x73)),
            block: Some(BlockNumber::new(2)),
            ledger_block: None,
        })
        .is_err());
}

#[test]
fn a_refused_minter_call_and_a_reverted_first_order_put_the_amount_back() {
    let mut world = World::new();
    let up = world.open(MoveDirection::IcpToBase, usdc(500), false);
    world
        .step(PoolStep::MoveWithdrawing {
            id: up,
            created_at: world.at(),
        })
        .unwrap();
    world
        .step(PoolStep::MoveRefused {
            id: up,
            why: "short of ckETH".to_string(),
            fee: TokenAmount::ZERO,
        })
        .unwrap();
    let icp = world.ledger().pockets.icp;
    assert_eq!(
        (icp.reserved, icp.in_flight),
        (usdc(500), TokenAmount::ZERO)
    );
    assert_eq!(world.item(up).status, MoveStatus::Open);

    let down = world.open(MoveDirection::BaseToIcp, usdc(700), false);
    world
        .step(PoolStep::MoveRouted {
            id: down,
            route: own(),
            order: order(3),
            scan_from: None,
        })
        .unwrap();
    world.step(signed(down, ChainId::BASE)).unwrap();
    world
        .step(PoolStep::MoveReverted {
            id: down,
            block: None,
        })
        .unwrap();
    let base = world.ledger().pockets.base;
    assert_eq!(
        (base.reserved, base.in_flight),
        (usdc(700), TokenAmount::ZERO)
    );
    // back to signing the order it pinned
    let item = world.item(down);
    assert_eq!(item.status, MoveStatus::Routed);
    assert_eq!(item.order, Some(order(3)));
    world.step(signed(down, ChainId::BASE)).unwrap();
    assert_eq!(world.ledger().pockets.base.in_flight, usdc(700));
}

/// Review Focus 3 from the fold's side: a stuck move keeps its lines where they are, a retry
/// sends it on from there, and a write-off takes it out of the assets.
#[test]
fn a_stuck_move_holds_its_lines_until_a_controller_retries_or_writes_it_off() {
    let mut world = World::new();
    let id = world.open(MoveDirection::BaseToIcp, usdc(1_000), false);
    world
        .step(PoolStep::MoveRouted {
            id,
            route: own(),
            order: order(1),
            scan_from: None,
        })
        .unwrap();
    world.step(signed(id, ChainId::BASE)).unwrap();
    world.step(burned(id, None)).unwrap();
    world
        .step(PoolStep::MoveStuck {
            id,
            why: "no arrival in three hours".to_string(),
        })
        .unwrap();
    assert_eq!(world.ledger().pockets.base.in_flight, usdc(1_000));
    assert_eq!(world.ledger().total_assets(), usdc(10_000));
    assert!(world
        .step(PoolStep::MoveStuck {
            id,
            why: "twice".to_string()
        })
        .is_err());
    world.step(PoolStep::MoveRetried { id }).unwrap();
    let item = world.item(id);
    assert_eq!((item.stuck, item.status), (None, MoveStatus::Burned));
    world
        .step(PoolStep::MoveLost {
            id,
            why: "the burn was never attested".to_string(),
        })
        .unwrap();
    assert_eq!(world.ledger().pockets.base.in_flight, TokenAmount::ZERO);
    assert_eq!(world.ledger().total_assets(), usdc(9_000));
    assert_eq!(world.ledger().moves().lost, usdc(1_000));
    assert!(world.ledger().moves().open.is_empty());
}

#[test]
fn a_retry_of_a_minter_call_a_controller_found_never_ran_asks_it_again() {
    let mut world = World::new();
    let id = world.open(MoveDirection::IcpToBase, usdc(500), false);
    world
        .step(PoolStep::MoveWithdrawing {
            id,
            created_at: world.at(),
        })
        .unwrap();
    world
        .step(PoolStep::MoveStuck {
            id,
            why: "the minter's answer was lost".to_string(),
        })
        .unwrap();
    world.step(PoolStep::MoveRetried { id }).unwrap();
    let icp = world.ledger().pockets.icp;
    assert_eq!(
        (icp.reserved, icp.in_flight),
        (usdc(500), TokenAmount::ZERO)
    );
    assert_eq!(world.item(id).status, MoveStatus::Open);
}

#[test]
fn an_unfilled_eco_move_is_refunded_and_routed_again() {
    let mut world = World::new();
    let id = world.open(MoveDirection::BaseToIcp, usdc(1_000), false);
    let fee = TokenAmount::from(100_000_u64);
    world
        .step(PoolStep::MoveProposed {
            id,
            order: order(4),
            fee,
        })
        .unwrap();
    world
        .step(PoolStep::MoveRouted {
            id,
            route: MoveRoute::Proposed { fee },
            order: order(4),
            scan_from: Some(BlockNumber::new(23_000_000)),
        })
        .unwrap();
    world.step(signed(id, ChainId::BASE)).unwrap();
    world.step(burned(id, Some([0x91; 32]))).unwrap();
    assert_eq!(world.item(id).intent, Some([0x91; 32]));
    // a refund of another amount is refused
    assert!(world
        .step(PoolStep::MoveRefunded {
            id,
            credit: key(0x92),
            amount: usdc(999),
            block: BlockNumber::new(50),
        })
        .is_err());
    world
        .step(PoolStep::MoveRefunded {
            id,
            credit: key(0x92),
            amount: usdc(1_000),
            block: BlockNumber::new(50),
        })
        .unwrap();
    let base = world.ledger().pockets.base;
    assert_eq!(
        (base.reserved, base.in_flight),
        (usdc(1_000), TokenAmount::ZERO)
    );
    assert!(world.pool.credited.contains(&key(0x92)));
    let item = world.item(id);
    assert_eq!(item.status, MoveStatus::Open);
    assert_eq!(
        (item.route, item.proposal.clone(), item.window_ends),
        (None, None, None)
    );
    assert!(item.routable());
}

#[test]
fn an_eco_fill_on_ethereum_lands_then_its_ck_deposit_is_signed_and_swept() {
    let mut world = World::new();
    let id = world.open(MoveDirection::BaseToIcp, usdc(1_000), false);
    let fee = TokenAmount::from(100_000_u64);
    world
        .step(PoolStep::MoveProposed {
            id,
            order: order(4),
            fee,
        })
        .unwrap();
    world
        .step(PoolStep::MoveRouted {
            id,
            route: MoveRoute::Proposed { fee },
            order: order(4),
            scan_from: Some(BlockNumber::new(23_000_000)),
        })
        .unwrap();
    world.step(signed(id, ChainId::BASE)).unwrap();
    world.step(burned(id, Some([0x91; 32]))).unwrap();
    // a sweep before the ck deposit is refused
    assert!(world
        .step(PoolStep::MoveSweeping {
            id,
            created_at: world.at(),
            amount: usdc(999),
            credited: usdc(999),
        })
        .is_err());
    let carried = TokenAmount::from(999_900_000_u64);
    world
        .step(PoolStep::MoveLanded {
            id,
            tx_hash: TxHash::new([0x93; 32]),
            block: BlockNumber::new(23_000_010),
            amount: carried,
            window_ends: None,
        })
        .unwrap();
    world.step(signed(id, ChainId::ETHEREUM)).unwrap();
    // the ck deposit moves no line: the move left Base with its order
    assert_eq!(world.ledger().pockets.base.in_flight, usdc(1_000));
    world.step(burned(id, None)).unwrap();
    // credited at most the amount less Eco's fee, whatever the subaccount holds
    assert!(world
        .step(PoolStep::MoveSweeping {
            id,
            created_at: world.at(),
            amount: usdc(5_000),
            credited: usdc(1_000),
        })
        .is_err());
    world
        .step(PoolStep::MoveSweeping {
            id,
            created_at: world.at(),
            amount: usdc(5_000),
            credited: carried,
        })
        .unwrap();
    world
        .step(PoolStep::MoveArrived {
            id,
            credited: carried,
            credit: None,
            block: None,
            ledger_block: Some(9),
        })
        .unwrap();
    assert_eq!(
        world.ledger().pockets.icp.free,
        usdc(4_000).checked_add(carried).unwrap()
    );
    assert_eq!(world.ledger().moves().cost, fee);
}

/// Review Focus 4 from the fold's side: one move per direction, the caps, the least move
/// unless a payout waits, behind the source pocket's queue.
#[test]
fn a_move_opens_only_within_its_rules() {
    let mut world = World::new();
    let at = world.at();
    // below the least move, unless urgent
    assert!(world
        .step(opened(0, MoveDirection::BaseToIcp, usdc(299), false, at))
        .is_err());
    // above the most one move carries
    assert!(world
        .step(opened(0, MoveDirection::BaseToIcp, usdc(5_001), true, at))
        .is_err());
    // more than the pocket holds behind its queue
    assert!(matches!(
        world.step(opened(0, MoveDirection::IcpToBase, usdc(4_001), true, at)),
        Err(PoolStepError::PocketShort { .. })
    ));
    // the wrong id
    assert!(matches!(
        world.step(opened(3, MoveDirection::BaseToIcp, usdc(400), false, at)),
        Err(PoolStepError::MoveId { expected: 0 })
    ));
    let first = world.open(MoveDirection::BaseToIcp, usdc(299), true);
    assert_eq!(first, 0);
    // one in flight that way: none more
    assert!(world
        .step(opened(1, MoveDirection::BaseToIcp, usdc(300), false, at))
        .is_err());
    // the other way is free
    let second = world.open(MoveDirection::IcpToBase, usdc(300), false);
    assert_eq!(second, 1);
}

#[test]
fn the_daily_cap_counts_every_move_opened_within_a_day() {
    let mut world = World::new();
    world.pool.ledger.pockets.base.free = usdc(50_000);
    world
        .step(PoolStep::MoveSettingsSet {
            settings: RebalanceSettings {
                daily_cap: usdc(6_000),
                ..RebalanceSettings::default()
            },
        })
        .unwrap();
    let first = world.open(MoveDirection::BaseToIcp, usdc(5_000), false);
    world
        .step(PoolStep::MoveLost {
            id: first,
            why: "a test".to_string(),
        })
        .unwrap();
    // its day is not over: 5,000 + 1,001 passes 6,000
    world.now += 3_600;
    let at = world.at();
    assert!(world
        .step(opened(1, MoveDirection::BaseToIcp, usdc(1_001), false, at))
        .is_err());
    world.open(MoveDirection::BaseToIcp, usdc(1_000), false);
    // a day later the first is out of it
    world.now += 86_400;
    world.pool.ledger.pockets.base.free = usdc(50_000);
    let lines = world.ledger().moves();
    assert_eq!(lines.opened_within_day(world.at()), TokenAmount::ZERO);
}

#[test]
fn a_proposal_is_taken_cheaper_within_its_cap_and_routes_only_as_taken() {
    let mut world = World::new();
    let id = world.open(MoveDirection::BaseToIcp, usdc(1_000), false);
    // 2 bps of $1,000 is 20 cents
    assert!(world
        .step(PoolStep::MoveProposed {
            id,
            order: order(1),
            fee: TokenAmount::from(200_001_u64),
        })
        .is_err());
    world
        .step(PoolStep::MoveProposed {
            id,
            order: order(1),
            fee: TokenAmount::from(150_000_u64),
        })
        .unwrap();
    // not cheaper: not taken
    assert!(world
        .step(PoolStep::MoveProposed {
            id,
            order: order(2),
            fee: TokenAmount::from(150_000_u64),
        })
        .is_err());
    world
        .step(PoolStep::MoveProposed {
            id,
            order: order(2),
            fee: TokenAmount::from(120_000_u64),
        })
        .unwrap();
    // routed only with the order and fee taken
    assert_eq!(
        world.step(PoolStep::MoveRouted {
            id,
            route: MoveRoute::Proposed {
                fee: TokenAmount::from(150_000_u64)
            },
            order: order(1),
            scan_from: None,
        }),
        Err(PoolStepError::Mismatch)
    );
    world
        .step(PoolStep::MoveRouted {
            id,
            route: MoveRoute::Proposed {
                fee: TokenAmount::from(120_000_u64),
            },
            order: order(2),
            scan_from: None,
        })
        .unwrap();
    // routed: no proposal more
    assert!(world
        .step(PoolStep::MoveProposed {
            id,
            order: order(3),
            fee: TokenAmount::from(1_u64),
        })
        .is_err());
    // an urgent move takes none
    world.pool.ledger.pockets.icp.free = usdc(4_000);
    let urgent = world.open(MoveDirection::IcpToBase, usdc(100), true);
    assert!(world
        .step(PoolStep::MoveProposed {
            id: urgent,
            order: order(4),
            fee: TokenAmount::from(1_u64),
        })
        .is_err());
}

#[test]
fn every_move_step_round_trips_and_is_named() {
    let steps = [
        PoolStep::MoveSettingsSet {
            settings: RebalanceSettings::default(),
        },
        opened(
            1,
            MoveDirection::IcpToBase,
            usdc(2),
            true,
            Timestamp::from_nanos(3),
        ),
        PoolStep::MoveProposed {
            id: 1,
            order: order(4),
            fee: TokenAmount::from(5_u64),
        },
        PoolStep::MoveRouted {
            id: 1,
            route: own(),
            order: order(6),
            scan_from: Some(BlockNumber::new(7)),
        },
        signed(1, ChainId::ETHEREUM),
        burned(1, Some([8; 32])),
        PoolStep::MoveReverted { id: 1, block: None },
        PoolStep::MoveWithdrawing {
            id: 1,
            created_at: Timestamp::from_nanos(9),
        },
        PoolStep::MoveWithdrawn {
            id: 1,
            withdrawal_id: 10,
            fee: TokenAmount::from(11_u64),
        },
        PoolStep::MoveRefused {
            id: 1,
            why: "no".to_string(),
            fee: TokenAmount::ZERO,
        },
        PoolStep::MoveLanded {
            id: 1,
            tx_hash: TxHash::new([12; 32]),
            block: BlockNumber::new(13),
            amount: usdc(14),
            window_ends: None,
        },
        PoolStep::MoveSweeping {
            id: 1,
            created_at: Timestamp::from_nanos(15),
            amount: usdc(16),
            credited: usdc(15),
        },
        PoolStep::MoveArrived {
            id: 1,
            credited: usdc(17),
            credit: Some(key(18)),
            block: Some(BlockNumber::new(19)),
            ledger_block: None,
        },
        PoolStep::MoveStuck {
            id: 1,
            why: "stuck".to_string(),
        },
        PoolStep::MoveRetried { id: 1 },
        PoolStep::MoveLost {
            id: 1,
            why: "lost".to_string(),
        },
        PoolStep::MoveRefunded {
            id: 1,
            credit: key(20),
            amount: usdc(21),
            block: BlockNumber::new(22),
        },
    ];
    for step in steps {
        assert_eq!(PoolStep::from_bytes(&step.to_bytes()), Some(step.clone()));
        assert!(step.name().starts_with("Move"));
        // a move's steps never arm the payouts' drive
        assert!(!step.moves_a_payout());
    }
}

/// P11e: an ICP to Base move whose Eco intent was refunded unfilled on Ethereum lands again,
/// routed again, its ckUSDC still in flight (never written off while it sits there); one the
/// ck minter reimbursed is back in the ICP pocket, closed; a minter call a controller retries
/// books the approval its lost answer may have given.
#[test]
fn an_unfilled_eco_move_lands_again_and_a_reimbursed_one_returns() {
    let mut world = World::new();
    let id = world.open(MoveDirection::IcpToBase, usdc(1_000), false);
    let fee = TokenAmount::from(100_000_u64);
    for step in [
        PoolStep::MoveWithdrawing {
            id,
            created_at: world.at(),
        },
        PoolStep::MoveWithdrawn {
            id,
            withdrawal_id: 1,
            fee: TokenAmount::ZERO,
        },
        PoolStep::MoveLanded {
            id,
            tx_hash: TxHash::new([0x61; 32]),
            block: BlockNumber::new(1),
            amount: usdc(1_000),
            window_ends: None,
        },
        PoolStep::MoveProposed {
            id,
            order: order(5),
            fee,
        },
    ] {
        // a proposal needs its window
        if let PoolStep::MoveProposed { .. } = step {
            let mut item = world.item(id);
            item.window_ends = Some(Timestamp::from_secs(world.now + 60).unwrap());
            world.moves.insert(id, item);
        }
        world.step(step).unwrap();
    }
    world
        .step(PoolStep::MoveRouted {
            id,
            route: MoveRoute::Proposed { fee },
            order: order(5),
            scan_from: None,
        })
        .unwrap();
    world.step(signed(id, ChainId::ETHEREUM)).unwrap();
    // not yet out: not unfilled
    assert!(world.step(PoolStep::MoveUnfilled { id }).is_err());
    world.step(burned(id, Some([0x95; 32]))).unwrap();
    world.step(PoolStep::MoveUnfilled { id }).unwrap();
    let item = world.item(id);
    assert_eq!(item.status, MoveStatus::Landed);
    assert_eq!(
        (item.route, item.intent, item.order.clone()),
        (None, None, None)
    );
    assert!(item.routable());
    assert_eq!(world.ledger().pockets.icp.in_flight, usdc(1_000));
}

#[test]
fn a_reimbursed_move_returns_to_the_icp_pocket_and_a_retried_call_books_its_approval() {
    let mut world = World::new();
    let id = world.open(MoveDirection::IcpToBase, usdc(1_000), false);
    world
        .step(PoolStep::MoveWithdrawing {
            id,
            created_at: world.at(),
        })
        .unwrap();
    world
        .step(PoolStep::MoveWithdrawn {
            id,
            withdrawal_id: 7,
            fee: TokenAmount::ZERO,
        })
        .unwrap();
    // never more than the move
    assert!(world
        .step(PoolStep::MoveReimbursed {
            id,
            amount: usdc(1_001),
            block: 9,
        })
        .is_err());
    world
        .step(PoolStep::MoveReimbursed {
            id,
            amount: usdc(1_000),
            block: 9,
        })
        .unwrap();
    let icp = world.ledger().pockets.icp;
    assert_eq!((icp.free, icp.in_flight), (usdc(4_000), TokenAmount::ZERO));
    assert!(world.item(id).status.is_final());
    assert!(world.ledger().moves().open.is_empty());

    // a minter call whose answer was lost, retried: one ckUSDC fee booked
    let again = world.open(MoveDirection::IcpToBase, usdc(500), false);
    world
        .step(PoolStep::MoveWithdrawing {
            id: again,
            created_at: world.at(),
        })
        .unwrap();
    world
        .step(PoolStep::MoveStuck {
            id: again,
            why: "the minter's answer was lost".to_string(),
        })
        .unwrap();
    let cost = world.ledger().moves().cost;
    world.step(PoolStep::MoveRetried { id: again }).unwrap();
    let fee = TokenAmount::from(super::super::tests::FEE);
    assert_eq!(world.ledger().moves().cost, cost.checked_add(fee).unwrap());
    assert_eq!(
        world.ledger().pockets.icp.free,
        usdc(3_500).checked_sub(fee).unwrap()
    );
}

/// L-11: an Eco move to ICP stopped as stuck after its hours, then refunded unfilled days
/// later, is routed again on its own: the refund clears the stuck mark, so no controller's
/// retry is needed.
#[test]
fn a_refunded_eco_move_is_no_longer_stuck() {
    let mut world = World::new();
    let id = world.open(MoveDirection::BaseToIcp, usdc(1_000), false);
    let fee = TokenAmount::from(100_000_u64);
    world
        .step(PoolStep::MoveProposed {
            id,
            order: order(4),
            fee,
        })
        .unwrap();
    world
        .step(PoolStep::MoveRouted {
            id,
            route: MoveRoute::Proposed { fee },
            order: order(4),
            scan_from: Some(BlockNumber::new(23_000_000)),
        })
        .unwrap();
    world.step(signed(id, ChainId::BASE)).unwrap();
    world.step(burned(id, Some([0x91; 32]))).unwrap();
    world
        .step(PoolStep::MoveStuck {
            id,
            why: "no Eco fill or ckUSDC arrived".to_string(),
        })
        .unwrap();
    assert!(world.item(id).stuck.is_some());
    world
        .step(PoolStep::MoveRefunded {
            id,
            credit: key(0x92),
            amount: usdc(1_000),
            block: BlockNumber::new(50),
        })
        .unwrap();
    let item = world.item(id);
    assert_eq!(item.stuck, None);
    assert_eq!(item.status, MoveStatus::Open);
    assert!(item.routable());
}

/// A withdrawal stopped for a controller still hears the minter: its landing lifts the stop,
/// so the move goes on with no controller; nothing else at `Withdrawn` does.
#[test]
fn a_late_landing_lifts_a_withdrawals_stop() {
    let mut world = World::new();
    let id = world.open(MoveDirection::IcpToBase, usdc(1_000), false);
    world
        .step(PoolStep::MoveWithdrawing {
            id,
            created_at: world.at(),
        })
        .unwrap();
    world
        .step(PoolStep::MoveWithdrawn {
            id,
            withdrawal_id: 9,
            fee: TokenAmount::ZERO,
        })
        .unwrap();
    world
        .step(PoolStep::MoveStuck {
            id,
            why: "a wait".to_string(),
        })
        .unwrap();
    assert!(world.item(id).stuck.is_some());
    world
        .step(PoolStep::MoveLanded {
            id,
            tx_hash: TxHash::new([0x71; 32]),
            block: BlockNumber::new(1),
            amount: usdc(1_000),
            window_ends: None,
        })
        .unwrap();
    let item = world.item(id);
    assert_eq!(item.status, MoveStatus::Landed);
    assert_eq!(item.stuck, None);
    assert!(item.routable());
}
