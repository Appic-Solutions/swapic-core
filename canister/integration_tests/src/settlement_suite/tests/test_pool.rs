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
    let (pic, canister, admin) = empty_canister();
    let arg = InitArg {
        config: Config {
            pool_chain: Some(BASE),
            ..init_arg().config
        },
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
    // the first answer was lost: the drive recorded it as a failure and asked again
    let failed = world.pool_steps().into_iter().any(
        |step| matches!(step, PoolStep::AttemptFailed { r#ref: failed, .. } if failed == r#ref),
    );
    assert!(failed, "the lost answer is in the log");
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
