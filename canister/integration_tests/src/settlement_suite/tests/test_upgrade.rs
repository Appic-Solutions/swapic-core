use crate::settlement_suite::init::{setup, setup_on, upgrade, watcher};
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

/// The fix round of 2026-11-08 (fix-canister 78a254c) upgrades in place from the deployed
/// build (`PRIOR_WASM`, its integration build of ck-part-6 398aeca): every line it wrote is read
/// by this build, its routes and its moves answer the same, the timers (the pool's own read of
/// Base among them, which now retries a failed read in 30 s) run five minutes over that state
/// with the canister answering throughout, and the new cycles door answers the watcher alone.
#[test]
#[ignore = "needs PRIOR_WASM: scripts/build-canister.sh --integration-test settlement at the deployed commit"]
fn the_deployed_state_survives_the_fix_round_upgrade() {
    let (pic, canister, admin) = setup_on(wasms::prior_settlement());
    seed_with_a_stopped_withdrawal(&pic, canister, admin);
    let events: u64 = read(&pic, canister, "event_count");
    let routes: IcpRoutesView = read(&pic, canister, "icp_routes");
    let moves: Vec<MoveView> = read(&pic, canister, "rebalance_moves");
    let stuck: Vec<MoveView> = read(&pic, canister, "rebalance_stuck");

    upgrade(&pic, canister, admin).unwrap();

    assert_eq!(read::<u64>(&pic, canister, "event_count"), events);
    assert_eq!(read::<IcpRoutesView>(&pic, canister, "icp_routes"), routes);
    assert_eq!(read::<Vec<MoveView>>(&pic, canister, "rebalance_moves"), moves);
    assert_eq!(read::<Vec<MoveView>>(&pic, canister, "rebalance_stuck"), stuck);

    for _ in 0..10 {
        pic.advance_time(std::time::Duration::from_secs(30));
        pic.tick();
    }
    assert_eq!(read::<Vec<MoveView>>(&pic, canister, "rebalance_stuck"), stuck);

    let cycles = |caller: Principal| -> Result<u64, String> {
        let raw = pic
            .query_call(canister, caller, "watcher_cycles_balance", encode_one(()).unwrap())
            .expect("the cycles door answers");
        decode_one(&raw).unwrap()
    };
    assert!(cycles(watcher()).is_ok_and(|balance| balance > 0), "the watcher reads the balance");
    assert!(cycles(admin).is_err(), "a controller is not the watcher");
    assert!(cycles(Principal::anonymous()).is_err(), "nobody else reads it");
}

/// The BNB Chain upgrade (bsc-canister) in place from the deployed build (`PRIOR_WASM`, its
/// integration build of 7a85358): every line it wrote is read by this build, its routes and
/// moves answer the same, and a config holding BNB Chain's Across rows written after it (the
/// integrator, AX13 and AX14's bounds, the private send, BNB Chain finalized, a chain's 18
/// decimal USDC and its Across pins) is read back as written, and again after the next upgrade.
#[test]
#[ignore = "needs PRIOR_WASM: scripts/build-canister.sh --integration-test settlement at the deployed commit"]
fn the_deployed_state_survives_the_bsc_upgrade_and_keeps_its_across_rows() {
    use crate::client::settlement::{get_config_full, set_config};
    use settlement_api::types::config::{AcrossFeeConfig, AcrossPinsConfig};
    let (pic, canister, admin) = setup_on(wasms::prior_settlement());
    seed_with_a_stopped_withdrawal(&pic, canister, admin);
    let events: u64 = read(&pic, canister, "event_count");
    let routes: IcpRoutesView = read(&pic, canister, "icp_routes");
    let moves: Vec<MoveView> = read(&pic, canister, "rebalance_moves");

    upgrade(&pic, canister, admin).unwrap();

    assert_eq!(read::<u64>(&pic, canister, "event_count"), events);
    assert_eq!(read::<IcpRoutesView>(&pic, canister, "icp_routes"), routes);
    assert_eq!(read::<Vec<MoveView>>(&pic, canister, "rebalance_moves"), moves);

    let mut config = get_config_full(&pic, canister, admin).unwrap();
    // the deployed build's config reads with every Across row absent
    assert_eq!(config.across_fee, None);
    assert_eq!(config.across_integrator, None);
    config.across_integrator = Some("0x00ab".to_string());
    config.across_fee = Some(AcrossFeeConfig {
        least_ppm: 50,
        most_ppm: 3_000,
        gas_allowance_usd6: std::collections::BTreeMap::from([(1, 500_000), (56, 100_000)]),
    });
    config.private_send_chains = Some(vec![56]);
    config.finalized_chains = Some(vec![137, 56]);
    // BNB Chain's entry as the kit writes it: no CCTP (its contracts the zero address), an
    // 18-decimal USDC, Across's pins, the relay_across row
    let zero = "0x0000000000000000000000000000000000000000".to_string();
    let swapic = "0xb3f66517bec4cc5a8e799db2dda4c810befcc917".to_string();
    let amounts = settlement_api::types::config::ActionAmounts {
        relay_across: Some(candid::Nat::from(100_000_000_000_000_u64)),
        across_claim: None,
        ..Default::default()
    };
    config.evm_chains.insert(
        56,
        settlement_api::types::config::EvmChainConfig {
            swapic: swapic.clone(),
            runner: swapic.clone(),
            cctp_receiver: swapic,
            usdc: "0x8AC76a51cc950d9822D68b83fE1Ad97B32Cd580d".to_string(),
            token_messenger: zero.clone(),
            message_transmitter: zero,
            gas_limit: settlement_api::types::config::ActionGas {
                relay_across: Some(1_200_000),
                across_claim: None,
                ..Default::default()
            },
            gas_budget_wei: amounts.clone(),
            min_network_fee: amounts,
            usdc_domain_name: None,
            usdc_domain_version: None,
            block_time_ms: 1_000,
            usdc_decimals: Some(18),
            across: Some(AcrossPinsConfig {
                periphery: "0x97CCDBea4632140639aD5eA9b944aa034eb15fD4".to_string(),
                swap_proxy: "0x1d1735bcf925d10b600279ce720da2673c2274c5".to_string(),
                spoke_pool: "0x4e8E101924eDE233C13e2D8622DC8aED2872d505".to_string(),
                multicall_handler: "0x0F7Ae28dE1C8532170AD4ee566B5801485c13a0E".to_string(),
            }),
        },
    );
    set_config(&pic, canister, admin, &config).unwrap();
    let written = get_config_full(&pic, canister, admin).unwrap();
    assert_eq!(written.across_fee, config.across_fee);
    assert_eq!(written.private_send_chains, Some(vec![56]));
    let bsc = &written.evm_chains[&56];
    assert_eq!(bsc.usdc_decimals, Some(18));
    assert!(bsc.across.is_some());
    assert_eq!(bsc.gas_limit.relay_across, Some(1_200_000));

    upgrade(&pic, canister, admin).unwrap();

    assert_eq!(get_config_full(&pic, canister, admin).unwrap(), written);
    assert_eq!(read::<u64>(&pic, canister, "event_count"), events + 1);
}

/// BNB Chain's wave 2 upgrade (U1b) in place from the deployed build (`PRIOR_WASM`, the
/// integration build of the live commit): every line it wrote is read by this build, its
/// payouts, routes and moves answer the same, the payouts' settings read with no Across cap;
/// then the lines and rows only this build writes (the Across cap, the claim's gas row, an
/// Across refund credited as the canister's own) are read back as written, and again after the
/// next upgrade.
#[test]
#[ignore = "needs PRIOR_WASM: scripts/build-canister.sh --integration-test settlement at the deployed commit"]
fn the_deployed_state_survives_the_bsc_wave2_upgrade_and_keeps_its_across_lines() {
    use crate::client::settlement::{get_config_full, set_config};
    use settlement_api::types::pool::{PayoutSettings, PoolError, PoolStateView};
    let (pic, canister, admin) = setup_on(wasms::prior_settlement());
    seed_with_a_stopped_withdrawal(&pic, canister, admin);
    let events: u64 = read(&pic, canister, "event_count");
    let routes: IcpRoutesView = read(&pic, canister, "icp_routes");
    let moves: Vec<MoveView> = read(&pic, canister, "rebalance_moves");
    let state: PoolStateView = read(&pic, canister, "pool_state");
    let payout = |pic: &PocketIc| -> Option<settlement_api::types::pool::PayoutView> {
        let raw = pic
            .query_call(canister, admin, "payout", encode_one(0_u64).unwrap())
            .unwrap();
        decode_one(&raw).unwrap()
    };
    let stopped = payout(&pic);

    upgrade(&pic, canister, admin).unwrap();

    assert_eq!(read::<u64>(&pic, canister, "event_count"), events);
    assert_eq!(read::<IcpRoutesView>(&pic, canister, "icp_routes"), routes);
    assert_eq!(read::<Vec<MoveView>>(&pic, canister, "rebalance_moves"), moves);
    assert_eq!(read::<PoolStateView>(&pic, canister, "pool_state"), state);
    assert_eq!(payout(&pic), stopped);

    // the payouts' settings with the Across cap, written by this build
    let settings = PayoutSettings {
        min_network_fee: candid::Nat::from(50_000_u32),
        order_wait_s: 120,
        max_payout_gas: 2_000_000,
        cctp_payouts: false,
        ck: None,
        across_in_flight_max: Some(candid::Nat::from(2_000_000_000_u64)),
    };
    let raw = pic
        .update_call(canister, admin, "set_payout_settings", encode_one(&settings).unwrap())
        .unwrap();
    assert_eq!(decode_one::<Result<(), PoolError>>(&raw).unwrap(), Ok(()));
    // the claim's gas row on Base
    let mut config = get_config_full(&pic, canister, admin).unwrap();
    if let Some(base) = config.evm_chains.get_mut(&8453) {
        base.gas_limit.across_claim = Some(150_000);
        base.gas_budget_wei.across_claim = Some(candid::Nat::from(10_000_000_000_000_u64));
        base.min_network_fee.across_claim = Some(candid::Nat::from(0_u8));
    }
    set_config(&pic, canister, admin, &config).unwrap();
    let written = get_config_full(&pic, canister, admin).unwrap();
    // an Across refund to the canister with no payout owed back: all of it its own surplus
    append_step(
        &pic,
        canister,
        admin,
        PoolStep::AcrossRefunded {
            credit: types::pool::CreditKey {
                chain: types::ChainId::BASE,
                tx: types::TxHash::new([0xa7; 32]),
                log_index: 2,
            },
            amount: TokenAmount::from(7_u8),
            block: types::BlockNumber::new(40),
            refunded: Vec::new(),
            surplus: TokenAmount::from(7_u8),
        },
    );
    let after = read::<u64>(&pic, canister, "event_count");
    assert!(after > events);

    upgrade(&pic, canister, admin).unwrap();

    assert_eq!(get_config_full(&pic, canister, admin).unwrap(), written);
    assert_eq!(read::<u64>(&pic, canister, "event_count"), after);
    let raw = pic
        .query_call(canister, admin, "payout_settings", encode_one(()).unwrap())
        .unwrap();
    let read_back: PayoutSettings = decode_one(&raw).unwrap();
    assert_eq!(read_back, settings);
}
