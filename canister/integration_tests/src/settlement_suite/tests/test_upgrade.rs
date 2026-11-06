use crate::settlement_suite::init::{setup, setup_on, upgrade};
use crate::wasms;
use candid::{decode_one, encode_one, Principal};
use pocket_ic::PocketIc;
use settlement_api::types::errors::TestAppendError;
use settlement_api::types::events::EventType;
use settlement_api::types::pool::{IcpRouteVia, IcpRoutesView, MoveStatus, MoveView};
use types::pool::{
    IcpAccount, IcpRoute, IcpRouteVia as Via, IcpRoutes, LedgerFee, MoveDirection, PoolAccount,
    PoolLedger, PoolSettings, PoolStep, PrincipalBytes, RebalanceSettings,
};
use types::{hash::PoolOpId, TokenAmount};

/// A fee the books earned on Base, `units` of USDC: a line the fold admits and folds.
fn fee(units: u64) -> EventType {
    EventType::from(types::EventType::Books {
        chain_id: types::ChainId::BASE,
        step: types::books::BookStep::FeeEarned {
            tx_hash: types::TxHash::new([units as u8; 32]),
            amount: types::TokenAmount::from(units),
            block: None,
        },
    })
}

#[test]
fn events_survive_upgrade() {
    let (pic, canister, admin) = setup();
    let raw = pic
        .query_call(canister, admin, "event_count", encode_one(()).unwrap())
        .unwrap();
    let installed = decode_one::<u64>(&raw).unwrap();
    let event = fee(1000);
    let raw = pic
        .update_call(canister, admin, "test_append", encode_one(&event).unwrap())
        .unwrap();
    decode_one::<Result<u64, TestAppendError>>(&raw)
        .unwrap()
        .unwrap();

    upgrade(&pic, canister, admin).unwrap();

    let raw = pic
        .query_call(canister, admin, "event_count", encode_one(()).unwrap())
        .unwrap();
    assert_eq!(decode_one::<u64>(&raw).unwrap(), installed + 1);
}

#[test]
fn test_append_rejects_non_controller() {
    let (pic, canister, _) = setup();
    let stranger = Principal::from_slice(&[9; 29]);
    let raw = pic
        .query_call(canister, stranger, "event_count", encode_one(()).unwrap())
        .unwrap();
    let installed = decode_one::<u64>(&raw).unwrap();
    let event = fee(1);
    let raw = pic
        .update_call(
            canister,
            stranger,
            "test_append",
            encode_one(&event).unwrap(),
        )
        .unwrap();
    assert!(decode_one::<Result<u64, TestAppendError>>(&raw)
        .unwrap()
        .is_err());

    // a rejected caller writes nothing
    let raw = pic
        .query_call(canister, stranger, "event_count", encode_one(()).unwrap())
        .unwrap();
    assert_eq!(decode_one::<u64>(&raw).unwrap(), installed);
}

fn append_step(pic: &PocketIc, canister: Principal, admin: Principal, step: PoolStep) {
    let line = EventType::from(types::EventType::Pool { step });
    let raw = pic
        .update_call(canister, admin, "test_append", encode_one(&line).unwrap())
        .unwrap();
    decode_one::<Result<u64, TestAppendError>>(&raw)
        .unwrap()
        .unwrap();
}

fn read<T: candid::CandidType + for<'de> candid::Deserialize<'de>>(
    pic: &PocketIc,
    canister: Principal,
    method: &str,
) -> T {
    let raw = pic
        .query_call(
            canister,
            Principal::anonymous(),
            method,
            encode_one(()).unwrap(),
        )
        .unwrap();
    decode_one(&raw).unwrap()
}

fn p(byte: u8) -> Principal {
    Principal::from_slice(&[byte; 10])
}

const CKUSDC: u8 = 1;
const ICP: u8 = 2;
const ICP_POOL: u8 = 3;
const GLDT: u8 = 4;
const GLDT_POOL: u8 = 5;
const VCHF: u8 = 6;
const VCHF_POOL: u8 = 7;
const CKXAUT: u8 = 8;
const CKXAUT_POOL: u8 = 9;

fn fee_of(byte: u8) -> LedgerFee {
    LedgerFee {
        ledger: p(byte),
        fee: TokenAmount::from(10_000_u32),
    }
}

/// The routes the deployed build can hold: a token direct, one through ICP.
fn deployed_routes() -> IcpRoutes {
    IcpRoutes {
        icp_ckusdc_pool: Some(PrincipalBytes(p(ICP_POOL))),
        icp_ledger: Some(PrincipalBytes(p(ICP))),
        routes: vec![
            IcpRoute {
                ledger: p(GLDT),
                via: Via::Direct(p(GLDT_POOL)),
            },
            IcpRoute {
                ledger: p(VCHF),
                via: Via::Icp(p(VCHF_POOL)),
            },
        ],
        ledger_fees: vec![fee_of(ICP), fee_of(GLDT), fee_of(VCHF)],
        pool_tokens: Vec::new(),
    }
}

/// [`deployed_routes`] and ckXAUT through GLDT.
fn with_hub() -> IcpRoutes {
    let mut routes = deployed_routes();
    routes.routes.push(IcpRoute {
        ledger: p(CKXAUT),
        via: Via::Hub {
            pool: p(CKXAUT_POOL),
            hub: p(GLDT),
        },
    });
    routes.ledger_fees.push(fee_of(CKXAUT));
    routes
}

/// The deployed build's state as prod could hold it, written by that build itself: a pool
/// with money in it, its routes, and a move to Base at each position this plan reads
/// differently: withdrawn (the landing), then stopped for a controller.
fn seed_with_a_stopped_withdrawal(pic: &PocketIc, canister: Principal, admin: Principal) {
    let step = |step| append_step(pic, canister, admin, step);
    step(PoolStep::SettingsSet {
        settings: PoolSettings {
            ckusdc_ledger: p(CKUSDC),
            open_for_swaps: true,
            ledger_fees: vec![fee_of(CKUSDC)],
            ..PoolSettings::default()
        },
    });
    let (id, amount) = (
        PoolOpId::new([0xee; 32]),
        TokenAmount::from(5_000_000_000_u64),
    );
    let account = IcpAccount {
        owner: p(0x20),
        subaccount: None,
    };
    let owner = PoolAccount::Icp(account);
    let now = types::Timestamp::from_nanos(pic.get_time().as_nanos_since_unix_epoch());
    step(PoolStep::StakeStarted {
        id,
        owner,
        from: account,
        amount,
        created_at: now,
        move_fee: TokenAmount::ZERO,
    });
    step(PoolStep::StakedIcp {
        id,
        owner,
        amount,
        move_fee: TokenAmount::ZERO,
        shares: PoolLedger::default().mint_now(amount).unwrap(),
        block: 1,
    });
    step(PoolStep::IcpRoutesSet {
        routes: deployed_routes(),
    });
    step(PoolStep::MoveSettingsSet {
        settings: RebalanceSettings::default(),
    });
    step(PoolStep::MoveOpened {
        id: 0,
        direction: MoveDirection::IcpToBase,
        amount: TokenAmount::from(1_000_000_000_u64),
        urgent: false,
        opened_at: now,
        window_ends: None,
        own_cost: TokenAmount::ZERO,
    });
    step(PoolStep::MoveWithdrawing {
        id: 0,
        created_at: now,
    });
    step(PoolStep::MoveWithdrawn {
        id: 0,
        withdrawal_id: 42,
        fee: TokenAmount::from(10_000_u32),
    });
    step(PoolStep::MoveStuck {
        id: 0,
        why: "the minter's USDC did not land on Ethereum".to_string(),
    });
}

/// Upgrades in place from the deployed build (`PRIOR_WASM`, its integration build of
/// 7c993d5): every line it wrote is read by this build, its routes and its open move answer
/// the same, and this build then stores a hub route, which survives its own upgrade.
#[test]
#[ignore = "needs PRIOR_WASM: scripts/build-canister.sh --integration-test settlement at the deployed commit"]
fn the_deployed_builds_state_survives_this_upgrade_and_takes_a_hub() {
    let (pic, canister, admin) = setup_on(wasms::prior_settlement());
    seed_with_a_stopped_withdrawal(&pic, canister, admin);
    let events: u64 = read(&pic, canister, "event_count");
    let routes: IcpRoutesView = read(&pic, canister, "icp_routes");
    let moves: Vec<MoveView> = read(&pic, canister, "rebalance_moves");
    let stuck: Vec<MoveView> = read(&pic, canister, "rebalance_stuck");
    assert_eq!(routes.routes.len(), 2);
    assert_eq!(moves.len(), 1);
    assert_eq!(stuck.len(), 1);
    // the build this starts from really is the old one: a hub route is beyond it
    let hub_line = EventType::from(types::EventType::Pool {
        step: PoolStep::IcpRoutesSet { routes: with_hub() },
    });
    let refused = pic.update_call(
        canister,
        admin,
        "test_append",
        encode_one(&hub_line).unwrap(),
    );
    let appended = refused
        .map(|raw| {
            decode_one::<Result<u64, TestAppendError>>(&raw)
                .unwrap()
                .is_ok()
        })
        .unwrap_or(false);
    assert!(!appended, "the prior build took a hub route");
    assert_eq!(read::<u64>(&pic, canister, "event_count"), events);

    upgrade(&pic, canister, admin).unwrap();

    assert_eq!(read::<u64>(&pic, canister, "event_count"), events);
    assert_eq!(read::<IcpRoutesView>(&pic, canister, "icp_routes"), routes);
    assert_eq!(
        read::<Vec<MoveView>>(&pic, canister, "rebalance_moves"),
        moves
    );
    assert_eq!(
        read::<Vec<MoveView>>(&pic, canister, "rebalance_stuck"),
        stuck
    );
    // the view shows a stopped move as its stop
    assert!(matches!(&moves[0].status, MoveStatus::Stuck(why) if why.contains("did not land")));

    // the timers run over the state the old build left, and the canister still answers
    for _ in 0..5 {
        pic.advance_time(std::time::Duration::from_secs(60));
        pic.tick();
    }
    assert_eq!(
        read::<Vec<MoveView>>(&pic, canister, "rebalance_stuck"),
        stuck
    );

    append_step(
        &pic,
        canister,
        admin,
        PoolStep::IcpRoutesSet { routes: with_hub() },
    );
    let hub = IcpRouteVia::Hub {
        pool: p(CKXAUT_POOL),
        hub: p(GLDT),
    };
    let read_hub = || {
        read::<IcpRoutesView>(&pic, canister, "icp_routes")
            .routes
            .iter()
            .find(|route| route.ledger == p(CKXAUT))
            .map(|route| route.via)
    };
    assert_eq!(read_hub(), Some(hub));
    upgrade(&pic, canister, admin).unwrap();
    assert_eq!(read_hub(), Some(hub));
    assert_eq!(read::<u64>(&pic, canister, "event_count"), events + 1);
}
