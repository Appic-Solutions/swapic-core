//! The pool's ICRC paths on a real ICRC-1/ICRC-2 ledger (the evm-minter's
//! `ledger_canister_u256`, as ckUSDC): a swap's payout, a lost answer asked again, a stake's
//! mint, and the ICP staker's stake and withdrawal. What happens on Base before an arrival
//! (the registration's checks, the canister's read of the receipt) is the unit tests'; here
//! those lines are planted through `test_append`, as the log would hold them.

use crate::client::pocket::{query, update};
use crate::client::settlement::append;
use crate::settlement_suite::init::{empty_canister, init_arg, install};
use crate::wasms;
use candid::{encode_one, CandidType, Nat, Principal};
use pocket_ic::PocketIc;
use settlement_api::types::config::Config;
use settlement_api::types::events::EventType;
use settlement_api::types::init::InitArg;
use settlement_api::types::pool::{
    PoolAccount, PoolAccountView, PoolDepositStatus, PoolDepositView, PoolError, PoolSettings,
    PoolStateView,
};
use std::time::Duration;
use types::books::{BalanceCheck, BookStep};
use types::hash::{PoolOpId, PoolRef, TxHash};
use types::pool::{CreditKey, DepositKind, IcpAccount, PoolRail, PoolStep, Registration, Shares};
use types::{BlockNumber, ChainId, Timestamp, TokenAmount};

const FEE: u64 = 10_000;
const BASE: u64 = 8453;

fn usdc(whole: u64) -> u64 {
    whole * 1_000_000
}

#[derive(CandidType)]
struct Account {
    owner: Principal,
    subaccount: Option<Vec<u8>>,
}

#[derive(CandidType)]
enum MetadataValue {
    #[allow(dead_code)]
    Text(String),
}

#[derive(CandidType)]
struct FeatureFlags {
    icrc2: bool,
}

#[derive(CandidType)]
struct ArchiveOptions {
    num_blocks_to_archive: u64,
    max_transactions_per_response: Option<u64>,
    trigger_threshold: u64,
    max_message_size_bytes: Option<u64>,
    cycles_for_archive_creation: Option<u64>,
    node_max_memory_size_bytes: Option<u64>,
    controller_id: Principal,
    more_controller_ids: Option<Vec<Principal>>,
}

#[derive(CandidType)]
struct LedgerInit {
    minting_account: Account,
    fee_collector_account: Option<Account>,
    transfer_fee: Nat,
    decimals: Option<u8>,
    max_memo_length: Option<u16>,
    token_symbol: String,
    token_name: String,
    metadata: Vec<(String, MetadataValue)>,
    initial_balances: Vec<(Account, Nat)>,
    feature_flags: Option<FeatureFlags>,
    maximum_number_of_accounts: Option<u64>,
    accounts_overflow_trim_quantity: Option<u64>,
    archive_options: ArchiveOptions,
}

#[derive(CandidType)]
enum LedgerArg {
    Init(LedgerInit),
}

fn account(owner: Principal) -> Account {
    Account {
        owner,
        subaccount: None,
    }
}

fn recipient() -> Principal {
    Principal::from_slice(&[0x51; 29])
}

fn staker() -> Principal {
    Principal::from_slice(&[0x52; 29])
}

/// The pool's world: the canister (pool chain Base) and a ckUSDC ledger that minted
/// `canister_usdc` to the canister and 1,000 USDC to the staker.
struct World {
    pic: PocketIc,
    canister: Principal,
    admin: Principal,
    ledger: Principal,
}

fn world(canister_usdc: u64) -> World {
    world_with(
        canister_usdc,
        Config {
            pool_chain: Some(BASE),
            ..init_arg().config
        },
    )
}

fn world_with(canister_usdc: u64, config: Config) -> World {
    let (pic, canister, admin) = empty_canister();
    let arg = InitArg {
        config,
        ..init_arg()
    };
    install(&pic, canister, admin, &arg).expect("installs");
    let ledger = pic.create_canister_with_settings(Some(admin), None);
    pic.add_cycles(ledger, 100_000_000_000_000);
    let init = LedgerArg::Init(LedgerInit {
        minting_account: account(Principal::from_slice(&[0x50; 29])),
        fee_collector_account: None,
        transfer_fee: Nat::from(FEE),
        decimals: Some(6),
        max_memo_length: Some(32),
        token_symbol: "ckUSDC".into(),
        token_name: "ckUSDC".into(),
        metadata: vec![],
        initial_balances: vec![
            (account(canister), Nat::from(canister_usdc)),
            (account(staker()), Nat::from(usdc(1_000))),
        ],
        feature_flags: Some(FeatureFlags { icrc2: true }),
        maximum_number_of_accounts: None,
        accounts_overflow_trim_quantity: None,
        archive_options: ArchiveOptions {
            num_blocks_to_archive: 1_000,
            max_transactions_per_response: None,
            trigger_threshold: 2_000,
            max_message_size_bytes: None,
            cycles_for_archive_creation: None,
            node_max_memory_size_bytes: None,
            controller_id: admin,
            more_controller_ids: None,
        },
    });
    pic.install_canister(
        ledger,
        wasms::icrc1_ledger(),
        encode_one(init).unwrap(),
        Some(admin),
    );
    let world = World {
        pic,
        canister,
        admin,
        ledger,
    };
    let settings = PoolSettings {
        tvl_cap: Nat::from(usdc(10_000)),
        per_swap_cap: Nat::from(usdc(1_000)),
        premium_bps: 10,
        ckusdc_ledger: ledger,
        icp_tokens: vec![],
        deposit_grace_s: 120,
        max_open_per_payer: 3,
        max_held_bps: 5_000,
        arrival_backup_s: 300,
        pool_open_for_swaps: true,
    };
    let set: Result<(), PoolError> = update(
        &world.pic,
        canister,
        admin,
        "set_pool_settings",
        encode_one(settings).unwrap(),
    );
    assert_eq!(set, Ok(()), "the settings read the ledger's fee");
    // the ICP pocket's first check, which the settings start
    world.settle();
    world.books_checked();
    world
}

impl World {
    fn now(&self) -> Timestamp {
        Timestamp::from_nanos(self.pic.get_time().as_nanos_since_unix_epoch())
    }

    fn plant(&self, step: PoolStep) {
        append(
            &self.pic,
            self.canister,
            self.admin,
            &EventType::from(types::EventType::Pool { step }),
        )
        .expect("the line is admitted");
    }

    /// A balance check of the pool chain, now: the pool's guard opens on it.
    fn books_checked(&self) {
        append(
            &self.pic,
            self.canister,
            self.admin,
            &EventType::from(types::EventType::Books {
                chain_id: ChainId::new(BASE),
                step: BookStep::Checked {
                    check: BalanceCheck {
                        block: BlockNumber::new(1),
                        observed: TokenAmount::ZERO,
                        expected: TokenAmount::ZERO,
                    },
                    shortfall: false,
                },
            }),
        )
        .expect("the check is admitted");
    }

    /// `amount` of the canister's own ckUSDC planted as staked on ICP by `owner`, the way
    /// `stake_icp` records it.
    fn plant_stake(&self, owner: Principal, amount: u64, shares: u128) {
        let id = PoolOpId::new([0xee; 32]);
        let owner = types::pool::PoolAccount::Icp(IcpAccount {
            owner,
            subaccount: None,
        });
        self.plant(PoolStep::StakeStarted {
            id,
            owner,
            from: IcpAccount {
                owner: self.canister,
                subaccount: None,
            },
            amount: TokenAmount::from(amount),
            created_at: self.now(),
        });
        self.plant(PoolStep::StakedIcp {
            id,
            owner,
            amount: TokenAmount::from(amount),
            rebalance_share: TokenAmount::ZERO,
            shares: Shares::new(shares),
            block: 0,
        });
    }

    /// A deposit registered and arrived on Base for 100 USDC, with a premium of 1.
    fn arrive(&self, kind: DepositKind, salt: u8) -> PoolRef {
        let swap = matches!(kind, DepositKind::Swap { .. });
        let terms = Registration {
            kind,
            min_usdc: TokenAmount::from(usdc(100)),
            premium: TokenAmount::from(if swap { usdc(1) } else { 0 }),
            rebalance_share: TokenAmount::ZERO,
            src_chain: ChainId::ARBITRUM,
            src_payer: "0xf00EBA8E5Fd231b526ADFAAa68B4d0ede72A89AC"
                .parse()
                .unwrap(),
            rail: PoolRail::Cctp,
            expires_at: self.now().checked_add(Duration::from_secs(600)).unwrap(),
            salt: [salt; 32],
        };
        let r#ref = terms.pool_ref().unwrap();
        self.plant(PoolStep::Registered {
            r#ref,
            deposit: terms,
        });
        self.plant(PoolStep::Arrived {
            r#ref,
            credit: CreditKey {
                chain: ChainId::new(BASE),
                tx: TxHash::new([salt; 32]),
                log_index: 1,
            },
            amount: TokenAmount::from(usdc(100)),
            block: BlockNumber::new(2),
        });
        r#ref
    }

    fn settle(&self) {
        for _ in 0..10 {
            self.pic.tick();
        }
    }

    /// Past the engine's backstop tick.
    fn next_tick(&self) {
        self.pic.advance_time(Duration::from_secs(31));
        self.settle();
    }

    fn balance(&self, owner: Principal) -> Nat {
        query(
            &self.pic,
            self.ledger,
            self.admin,
            "icrc1_balance_of",
            encode_one(account(owner)).unwrap(),
        )
    }

    fn deposit(&self, r#ref: PoolRef) -> PoolDepositView {
        let view: Option<PoolDepositView> = query(
            &self.pic,
            self.canister,
            self.admin,
            "pool_deposit",
            encode_one(r#ref.into_bytes().to_vec()).unwrap(),
        );
        view.expect("registered")
    }

    fn state(&self) -> PoolStateView {
        query(
            &self.pic,
            self.canister,
            self.admin,
            "pool_state",
            encode_one(()).unwrap(),
        )
    }

    fn pool_account(&self, owner: Principal) -> PoolAccountView {
        query(
            &self.pic,
            self.canister,
            self.admin,
            "pool_account",
            encode_one(PoolAccount::Icp(settlement_api::types::pool::IcpAccount {
                owner,
                subaccount: None,
            }))
            .unwrap(),
        )
    }

    /// Every pool step in the log, in order.
    fn pool_steps(&self) -> Vec<PoolStep> {
        let count = crate::client::settlement::event_count(&self.pic, self.canister, self.admin);
        crate::client::settlement::events_page(&self.pic, self.canister, self.admin, 0, count)
            .into_iter()
            .filter_map(|event| match event.payload {
                EventType::Pool { step_bytes, .. } => PoolStep::from_bytes(&step_bytes),
                _ => None,
            })
            .collect()
    }

    fn replay_matches(&self) -> bool {
        query(
            &self.pic,
            self.canister,
            self.admin,
            "verify_replay",
            encode_one(()).unwrap(),
        )
    }
}

fn ckusdc_swap(ledger: Principal) -> DepositKind {
    DepositKind::Swap {
        recipient: IcpAccount {
            owner: recipient(),
            subaccount: None,
        },
        token_out: ledger,
        min_out: 1,
    }
}

/// A registered swap whose arrival is recorded pays its recipient exactly
/// `arrived - premium` in ckUSDC, on the canister's own drive.
#[test]
fn an_arrived_swap_pays_its_recipient_arrived_less_premium() {
    let world = world(usdc(1_000));
    world.plant_stake(world.admin, usdc(1_000), 1);
    assert!(world.state().open, "{:?}", world.state().closed_reason);
    let r#ref = world.arrive(ckusdc_swap(world.ledger), 1);
    world.settle();
    assert_eq!(world.balance(recipient()), Nat::from(usdc(99)));
    let view = world.deposit(r#ref);
    assert_eq!(view.status, PoolDepositStatus::Paid);
    assert_eq!(view.paid, Some(Nat::from(usdc(99))));
    // the pocket paid the recipient and the ledger's fee, and holds nothing back for it
    let state = world.state();
    assert_eq!(state.icp.reserved, Nat::from(0_u8));
    assert_eq!(state.icp.free, Nat::from(usdc(901) - FEE));
    assert_eq!(world.balance(world.canister), Nat::from(usdc(901) - FEE));
    assert!(world.replay_matches());
}

/// Review Focus 2: the transfer executes and its answer is lost; the next drive asks again
/// with the same `created_at_time` and memo, the ledger answers `Duplicate`, and the
/// recipient is paid once.
#[test]
fn a_lost_answer_asked_again_pays_once() {
    let world = world(usdc(1_000));
    world.plant_stake(world.admin, usdc(1_000), 1);
    let lost: Result<(), settlement_api::types::errors::GuardError> = update(
        &world.pic,
        world.canister,
        world.admin,
        "test_pool_lose_next_answer",
        encode_one(()).unwrap(),
    );
    assert_eq!(lost, Ok(()));
    let r#ref = world.arrive(ckusdc_swap(world.ledger), 2);
    world.settle();
    world.next_tick();
    // the first answer was lost: asked again under the one dedup pair its start recorded,
    // as an outcome unknown, which records nothing and counts no attempt
    let steps = world.pool_steps();
    let starts = steps
        .iter()
        .filter(|step| matches!(step, PoolStep::PayoutStarted { r#ref: started, .. } if *started == r#ref))
        .count();
    assert_eq!(starts, 1);
    assert!(!steps
        .iter()
        .any(|step| matches!(step, PoolStep::AttemptFailed { .. })));
    // and the knob fired: the ledger executed the first transfer and its answer was lost
    let lost: u64 = query(
        &world.pic,
        world.canister,
        world.admin,
        "test_pool_answers_lost",
        encode_one(()).unwrap(),
    );
    assert_eq!(lost, 1);
    let view = world.deposit(r#ref);
    assert_eq!(view.status, PoolDepositStatus::Paid);
    assert_eq!(world.balance(recipient()), Nat::from(usdc(99)), "paid once");
    assert_eq!(world.balance(world.canister), Nat::from(usdc(901) - FEE));
    assert!(world.replay_matches());
}

/// An arrived stake mints its shares at the pool's price before it, and calls no ledger.
#[test]
fn an_arrived_stake_mints_its_shares() {
    let world = world(usdc(1_000));
    world.plant_stake(world.admin, usdc(1_000), 1_000_000_000_000_000_000_000);
    let owner = types::pool::PoolAccount::Icp(IcpAccount {
        owner: staker(),
        subaccount: None,
    });
    let r#ref = world.arrive(DepositKind::Stake { owner }, 3);
    world.settle();
    let view = world.deposit(r#ref);
    assert_eq!(view.status, PoolDepositStatus::Paid);
    // 100 USDC into 1,000 USDC behind 10^21 shares: 10^20
    let minted = Nat::from(100_000_000_000_000_000_000_u128);
    assert_eq!(view.shares_minted, Some(minted.clone()));
    let mine = world.pool_account(staker());
    assert_eq!(mine.shares, minted);
    assert_eq!(mine.value_usdc, Nat::from(usdc(100)));
    assert!(world.replay_matches());
}

#[derive(CandidType)]
struct ApproveArgs {
    from_subaccount: Option<Vec<u8>>,
    spender: Account,
    amount: Nat,
    expected_allowance: Option<Nat>,
    expires_at: Option<u64>,
    fee: Option<Nat>,
    memo: Option<Vec<u8>>,
    created_at_time: Option<u64>,
}

impl World {
    /// `owner` approves the canister's main account for `amount`.
    fn approve(&self, owner: Principal, amount: u64) {
        let args = ApproveArgs {
            from_subaccount: None,
            spender: account(self.canister),
            amount: Nat::from(amount),
            expected_allowance: None,
            expires_at: None,
            fee: None,
            memo: None,
            created_at_time: None,
        };
        let raw = self
            .pic
            .update_call(
                self.ledger,
                owner,
                "icrc2_approve",
                encode_one(args).unwrap(),
            )
            .expect("the ledger answers");
        assert!(!raw.is_empty());
    }
}

/// The ICP staker's round trip: stake 100 ckUSDC on its own approval, a swap earns the pool
/// its premium, then withdraw all of it: the shares pay at most what they are worth, every
/// ledger fee out of the withdrawal, and the pool's lines match the ledger.
#[test]
fn an_icp_staker_stakes_earns_and_withdraws_at_most_its_value() {
    use settlement_api::types::pool::{StakeIcpArgs, StakeIcpOk, WithdrawIcpArgs, WithdrawIcpOk};
    let world = world(usdc(1_000));
    world.plant_stake(world.admin, usdc(1_000), 1_000_000_000_000_000_000_000);
    world.approve(staker(), usdc(100) + FEE);
    let staked: Result<StakeIcpOk, PoolError> = update(
        &world.pic,
        world.canister,
        staker(),
        "stake_icp",
        encode_one(StakeIcpArgs {
            from_subaccount: None,
            amount: Nat::from(usdc(100)),
            owner: PoolAccount::Icp(settlement_api::types::pool::IcpAccount {
                owner: staker(),
                subaccount: None,
            }),
        })
        .unwrap(),
    );
    let shares = staked.expect("staked").shares;
    assert_eq!(shares, Nat::from(100_000_000_000_000_000_000_u128));
    // the approval's fee and the pull's are the staker's
    assert_eq!(world.balance(staker()), Nat::from(usdc(900) - 2 * FEE));
    assert_eq!(world.balance(world.canister), Nat::from(usdc(1_100)));

    // a swap of 100 pays 99 and its fee from ICP; its premium stays with the stakers
    world.arrive(ckusdc_swap(world.ledger), 4);
    world.settle();
    let value = world.pool_account(staker()).value_usdc;
    assert!(
        value > usdc(100),
        "the staker earned part of the premium: {value}"
    );

    let withdrawn: Result<WithdrawIcpOk, PoolError> = update(
        &world.pic,
        world.canister,
        staker(),
        "withdraw_icp",
        encode_one(WithdrawIcpArgs {
            from: None,
            usdc: value.clone(),
            max_shares: shares.clone(),
            token_out: world.ledger,
            min_out: Nat::from(0_u8),
            to: settlement_api::types::pool::IcpAccount {
                owner: staker(),
                subaccount: None,
            },
        })
        .unwrap(),
    );
    let withdrawn = withdrawn.expect("withdrawn");
    assert!(withdrawn.burned <= shares);
    assert_eq!(withdrawn.paid, value.clone() - Nat::from(FEE));
    assert!(withdrawn.paid <= value);
    assert_eq!(
        world.balance(staker()),
        Nat::from(usdc(900) - 2 * FEE) + withdrawn.paid.clone()
    );
    // what is left of its shares is dust worth nothing
    assert!(world.pool_account(staker()).value_usdc <= 1_u8);
    // the ICP pocket's lines are its balance on the ledger
    let state = world.state();
    assert_eq!(
        state.icp.free.clone() + state.icp.reserved.clone(),
        world.balance(world.canister)
    );
    assert!(world.replay_matches());

    // more than its shares are worth: refused, nothing burned
    let again: Result<WithdrawIcpOk, PoolError> = update(
        &world.pic,
        world.canister,
        staker(),
        "withdraw_icp",
        encode_one(WithdrawIcpArgs {
            from: None,
            usdc: Nat::from(usdc(1)),
            max_shares: shares,
            token_out: world.ledger,
            min_out: Nat::from(0_u8),
            to: settlement_api::types::pool::IcpAccount {
                owner: staker(),
                subaccount: None,
            },
        })
        .unwrap(),
    );
    assert!(
        matches!(again, Err(PoolError::ShortShares { .. })),
        "{again:?}"
    );
}

/// The ICP pocket's check: lines the ledger does not hold close the pool, and a controller's
/// reopening opens it again.
#[test]
fn an_icp_shortfall_closes_the_pool_and_reopening_clears_it() {
    // the pool's lines say 1,000 on ICP, and the ledger holds 500
    let world = world(usdc(500));
    world.plant_stake(world.admin, usdc(1_000), 1);
    assert!(world.state().open);
    // the books' check, every ten minutes
    world.pic.advance_time(Duration::from_secs(601));
    world.settle();
    let state = world.state();
    assert!(!state.open);
    assert_eq!(
        state.closed_reason.as_deref(),
        Some("the balance check of the ICP pocket found a shortfall")
    );
    let reopened: Result<(), String> = update(
        &world.pic,
        world.canister,
        world.admin,
        "set_pool_paused",
        encode_one(false).unwrap(),
    );
    assert_eq!(reopened, Ok(()));
    assert!(world.state().open, "{:?}", world.state().closed_reason);
    assert!(world.replay_matches());
}

/// Base as the pool pays on it: Swapic, the Runner, USDC, and the pool's gas rows.
fn base_chain() -> settlement_api::types::config::EvmChainConfig {
    use settlement_api::types::config::{ActionAmounts, ActionGas, EvmChainConfig};
    let amounts = |value: u64| ActionAmounts {
        relay_cctp: Nat::from(value),
        relay_eco: Nat::from(value),
        relay_local: Nat::from(value),
        complete_swap: Nat::from(value),
        complete_ck: Nat::from(value),
        relay_deliver: Nat::from(value),
        release: Nat::from(value),
        pool_payout: Some(Nat::from(value)),
        pool_transfer: Some(Nat::from(value)),
        pool_approve: Some(Nat::from(value)),
        pool_move: None,
        pool_move_finish: None,
        pool_ck_deposit: None,
    };
    EvmChainConfig {
        swapic: "0xF0Ba68cBc87c31eCaA9654EB14daF3632eaCa406".to_string(),
        runner: "0x7e345aca00000000000000000000000000000001".to_string(),
        cctp_receiver: "0x7e345aca00000000000000000000000000000002".to_string(),
        usdc: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".to_string(),
        token_messenger: "0x28b5a0e9C621a5BadaA536219b3a228C8168cf5d".to_string(),
        message_transmitter: "0x81D40F21F12A8F0E3252Bccb954D722d4c464B64".to_string(),
        gas_limit: ActionGas {
            relay_cctp: 600_000,
            relay_eco: 600_000,
            relay_local: 900_000,
            complete_swap: 1_300_000,
            complete_ck: 600_000,
            relay_deliver: 300_000,
            release: 300_000,
            pool_payout: Some(1_000_000),
            pool_transfer: Some(100_000),
            pool_approve: Some(100_000),
            pool_move: None,
            pool_move_finish: None,
            pool_ck_deposit: None,
        },
        gas_budget_wei: amounts(100_000_000_000_000_000),
        min_network_fee: amounts(0),
    }
}

/// P11b Task 3: an ICP user swaps 100 ckUSDC to USDC on Base through the pool. The canister
/// pulls it on the user's approval (the user pays the pull's fee), knows the exact amount at
/// once, and the payout waits for its order with `100 - premium - network fee` reserved on
/// Base. A stake planted on Base covers it.
#[test]
fn an_icp_user_swaps_ckusdc_to_evm_and_the_payout_waits_with_its_exact_amount() {
    use settlement_api::types::pool::{
        IcpSwapTerms, PayoutSettings, PayoutStatus, PayoutTerms, PayoutView, SwapFromIcpOk,
    };
    let world = world_with(
        usdc(1_000),
        Config {
            pool_chain: Some(BASE),
            evm_chains: std::collections::BTreeMap::from([(BASE, base_chain())]),
            ..init_arg().config
        },
    );
    world.plant_stake(world.admin, usdc(1_000), 1_000_000_000_000_000_000_000);
    // the Base pocket: 500 USDC staked from Base, arrived and minted at the pool's price
    let owner = types::pool::PoolAccount::Evm(
        "0x1111111111111111111111111111111111111111"
            .parse()
            .unwrap(),
    );
    let stake = Registration {
        kind: DepositKind::Stake { owner },
        min_usdc: TokenAmount::from(usdc(500)),
        premium: TokenAmount::ZERO,
        rebalance_share: TokenAmount::ZERO,
        src_chain: ChainId::new(BASE),
        src_payer: "0xf00EBA8E5Fd231b526ADFAAa68B4d0ede72A89AC"
            .parse()
            .unwrap(),
        rail: PoolRail::SameChain,
        expires_at: world.now().checked_add(Duration::from_secs(600)).unwrap(),
        salt: [0x71; 32],
    };
    let r#ref = stake.pool_ref().unwrap();
    world.plant(PoolStep::Registered {
        r#ref,
        deposit: stake,
    });
    world.plant(PoolStep::Arrived {
        r#ref,
        credit: CreditKey {
            chain: ChainId::new(BASE),
            tx: TxHash::new([0x71; 32]),
            log_index: 1,
        },
        amount: TokenAmount::from(usdc(500)),
        block: BlockNumber::new(2),
    });
    world.plant(PoolStep::Minted {
        r#ref,
        owner,
        shares: Shares::new(500_000_000_000_000_000_000),
    });
    let set: Result<(), PoolError> = update(
        &world.pic,
        world.canister,
        world.admin,
        "set_payout_settings",
        encode_one(PayoutSettings {
            min_network_fee: Nat::from(50_000_u32),
            rebalance_share_floor_bps: 2,
            order_wait_s: 120,
            max_payout_gas: 2_000_000,
        })
        .unwrap(),
    );
    assert_eq!(set, Ok(()));
    world.books_checked();
    world.approve(staker(), usdc(100) + FEE);
    let deadline = world.now().checked_add(Duration::from_secs(900)).unwrap();
    let terms = IcpSwapTerms {
        token_in: world.ledger,
        amount_in: Nat::from(usdc(100)),
        from_subaccount: None,
        min_usdc: Nat::from(usdc(100)),
        premium: Nat::from(usdc(1)),
        network_fee: Nat::from(50_000_u32),
        payout: PayoutTerms {
            dst_chain: BASE,
            recipient: "0x2222222222222222222222222222222222222222".to_string(),
            token_out: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".to_string(),
            min_out: Nat::from(usdc(98)),
            deadline_ns: deadline.as_nanos(),
        },
        refund_to: None,
    };
    let canister_before = world.balance(world.canister);
    let swapped: Result<SwapFromIcpOk, PoolError> = update(
        &world.pic,
        world.canister,
        staker(),
        "swap_from_icp",
        encode_one(terms).unwrap(),
    );
    let id = swapped.expect("the swap is opened").id;
    // pulled: the canister holds 100 more, the user paid 100, its approval's fee and the
    // pull's
    assert_eq!(world.balance(world.canister), canister_before + usdc(100));
    assert_eq!(
        world.balance(staker()),
        Nat::from(usdc(1_000) - usdc(100) - 2 * FEE)
    );
    let view: Option<PayoutView> = query(
        &world.pic,
        world.canister,
        world.admin,
        "payout",
        encode_one(id).unwrap(),
    );
    let view = view.expect("the payout is there");
    assert_eq!(view.status, PayoutStatus::Waiting);
    assert_eq!(view.amount, Some(Nat::from(usdc(100) - usdc(1) - 50_000)));
    let state = world.state();
    assert_eq!(state.base.reserved, Nat::from(usdc(100) - usdc(1) - 50_000));
    assert!(world.replay_matches());
}
