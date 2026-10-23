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
    world_in(pic, canister, admin, canister_usdc, config)
}

/// The pool's world on a network holding the test threshold keys (the canister on an
/// application subnet, the keys on the II subnet): what signing needs.
fn signing_world(canister_usdc: u64, config: Config) -> World {
    let pic = pocket_ic::PocketIcBuilder::new()
        .with_ii_subnet()
        .with_application_subnet()
        .build();
    let admin = Principal::from_slice(&[1; 29]);
    let subnet = pic.topology().get_app_subnets()[0];
    let canister = pic.create_canister_on_subnet(Some(admin), None, subnet);
    pic.add_cycles(canister, 1_000_000_000_000_000);
    world_in(pic, canister, admin, canister_usdc, config)
}

fn world_in(
    pic: PocketIc,
    canister: Principal,
    admin: Principal,
    canister_usdc: u64,
    config: Config,
) -> World {
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
        arrival_backup_s: 300,
        pool_open_for_swaps: true,
        large_deposit_usdc: None,
        large_deposit_confirmations: None,
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

    /// A deposit arrived on Base for 100 USDC with its terms (P11d: no registration), a swap
    /// with a premium of 1 and one ckUSDC fee of ICP fees, its own.
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
            icp_fees: swap.then_some(TokenAmount::from(FEE)),
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

/// A swap whose arrival is recorded pays its recipient exactly `arrived - premium - fee` in
/// ckUSDC, on the canister's own drive: the user pays every fee (P11d).
#[test]
fn an_arrived_swap_pays_its_recipient_arrived_less_premium() {
    let world = world(usdc(1_000));
    world.plant_stake(world.admin, usdc(1_000), 1);
    assert!(world.state().open, "{:?}", world.state().closed_reason);
    let r#ref = world.arrive(ckusdc_swap(world.ledger), 1);
    world.settle();
    assert_eq!(world.balance(recipient()), Nat::from(usdc(99) - FEE));
    let view = world.deposit(r#ref);
    assert_eq!(view.status, PoolDepositStatus::Paid);
    assert_eq!(view.paid, Some(Nat::from(usdc(99) - FEE)));
    // the pocket paid the recipient and the ledger's fee out of the user's 99, and holds
    // nothing back for it
    let state = world.state();
    assert_eq!(state.icp.reserved, Nat::from(0_u8));
    assert_eq!(state.icp.free, Nat::from(usdc(901)));
    assert_eq!(world.balance(world.canister), Nat::from(usdc(901)));
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
    assert_eq!(
        world.balance(recipient()),
        Nat::from(usdc(99) - FEE),
        "paid once"
    );
    assert_eq!(world.balance(world.canister), Nat::from(usdc(901)));
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

    // a swap of 100 pays 99 from ICP, its fee out of it; its premium stays with the stakers
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
pub(crate) fn base_chain() -> settlement_api::types::config::EvmChainConfig {
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
        eco_rescue: None,
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
            eco_rescue: None,
        },
        gas_budget_wei: amounts(100_000_000_000_000_000),
        min_network_fee: amounts(0),
    }
}

/// The payout plan (2026-10-23): a plan for `payout`, USDC on Base paid to `to` at least 1
/// USDC, no step, paid by the canister at `canister`.
fn usdc_plan(canister: types::EvmAddress, payout: u64, deadline_s: u64) -> Vec<u8> {
    use alloy_primitives::{FixedBytes, U256};
    use types::swapic::{addr, encode_order, Order, Payout};
    let base_usdc: types::EvmAddress = base_chain().usdc.parse().unwrap();
    let to: types::EvmAddress = "0x2222222222222222222222222222222222222222"
        .parse()
        .unwrap();
    encode_order(&Order {
        r#ref: FixedBytes([0x61; 32]),
        payer: addr(canister),
        tokenIn: addr(base_usdc),
        amountIn: U256::from(payout),
        deadline: U256::from(deadline_s),
        steps: vec![],
        payout: Payout {
            token: addr(base_usdc),
            recipient: addr(to),
            minOut: U256::from(usdc(1)),
        },
    })
}

/// The deployed Runner on Base.
const RUNNER: &str = "0x7E345acaB79102ac9588C60B9358Ec4d79438eEe";

/// A world whose Base pocket holds 1,000 USDC staked from Base and the payout settings, its
/// canister's EVM address derived on the test key: what an ICP swap to EVM needs.
fn swap_world() -> (World, types::EvmAddress) {
    use settlement_api::types::pool::PayoutSettings;
    let world = signing_world(
        usdc(1_000),
        Config {
            pool_chain: Some(BASE),
            evm_chains: std::collections::BTreeMap::from([(
                BASE,
                settlement_api::types::config::EvmChainConfig {
                    // the deployed Runner: the real KyberSwap route pays it
                    runner: RUNNER.to_string(),
                    ..base_chain()
                },
            )]),
            rpc_urls: std::collections::BTreeMap::from([(
                BASE,
                "https://base-mainnet.example/v2/key".to_string(),
            )]),
            ..init_arg().config
        },
    );
    let address =
        crate::client::settlement::derive_evm_address(&world.pic, world.canister, world.admin)
            .expect("the test key derives an address")
            .parse()
            .expect("an EVM address");
    world.plant_stake(world.admin, usdc(1_000), 1_000_000_000_000_000_000_000);
    // the Base pocket: 500 USDC staked from Base, arrived and minted at the pool's price
    let owner = types::pool::PoolAccount::Evm(
        "0x1111111111111111111111111111111111111111"
            .parse()
            .unwrap(),
    );
    let stake = Registration {
        kind: DepositKind::Stake { owner },
        min_usdc: TokenAmount::from(usdc(1_000)),
        premium: TokenAmount::ZERO,
        rebalance_share: TokenAmount::ZERO,
        src_chain: ChainId::new(BASE),
        src_payer: "0xf00EBA8E5Fd231b526ADFAAa68B4d0ede72A89AC"
            .parse()
            .unwrap(),
        rail: PoolRail::SameChain,
        expires_at: world.now().checked_add(Duration::from_secs(600)).unwrap(),
        salt: [0x71; 32],
        icp_fees: None,
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
        amount: TokenAmount::from(usdc(1_000)),
        block: BlockNumber::new(2),
    });
    world.plant(PoolStep::Minted {
        r#ref,
        owner,
        shares: Shares::new(1_000_000_000_000_000_000_000),
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
            cctp_min_network_fee: None,
        })
        .unwrap(),
    );
    assert_eq!(set, Ok(()));
    world.books_checked();
    (world, address)
}

/// The swap's arguments: 100 ckUSDC with `plan`, 100 bps of slippage, its deadline 15
/// minutes away.
fn swap_terms(
    world: &World,
    amount_in: u64,
    plan: Vec<u8>,
    gas_limit: u64,
) -> settlement_api::types::pool::IcpSwapTerms {
    use settlement_api::types::pool::{IcpSwapTerms, PayoutPlan};
    IcpSwapTerms {
        token_in: world.ledger,
        amount_in: Nat::from(amount_in),
        from_subaccount: None,
        refund_to: None,
        slippage_bps: 100,
        deadline_ns: world
            .now()
            .checked_add(Duration::from_secs(900))
            .unwrap()
            .as_nanos(),
        plan: PayoutPlan {
            order: plan,
            gas_limit,
        },
    }
}

/// KyberSwap's InputScalingHelper on Base.
const SCALER: &str = "0x2f577A41BeC1BE1152AeEA12e73b7391d15f655D";

/// The real KyberSwap route on Base for 500 USDC to WETH, the deployed Runner its receiver,
/// and the scaler's own answers for it, read on Base for lane C1 (2026-10-23).
fn scaler_sample() -> serde_json::Value {
    serde_json::from_str(include_str!(
        "../../../../impl/src/pool/plan/samples/scaler_base.json"
    ))
    .unwrap()
}

fn sample_hex(value: &serde_json::Value) -> Vec<u8> {
    hex::decode(value.as_str().unwrap().trim_start_matches("0x")).unwrap()
}

/// The scaler's raw answer for `amount`.
fn scaler_answer(amount: u64) -> Vec<u8> {
    let sample = scaler_sample();
    let found = sample["answers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|answer| {
            answer["new_amount"]
                .as_str()
                .and_then(|text| text.parse().ok())
                == Some(amount)
        })
        .unwrap()
        .clone();
    sample_hex(&found["result"])
}

/// A payout plan for 500 USDC through the real route to WETH on Base, at least 0.2 WETH to
/// the recipient, paid by the canister at `canister`.
fn kyber_plan(canister: types::EvmAddress, deadline_s: u64) -> Vec<u8> {
    use alloy_primitives::{FixedBytes, U256};
    use types::swapic::{addr, encode_order, Order, Payout, Step, KYBER_ROUTER};
    let base_usdc: types::EvmAddress = base_chain().usdc.parse().unwrap();
    let to: types::EvmAddress = "0x2222222222222222222222222222222222222222"
        .parse()
        .unwrap();
    encode_order(&Order {
        r#ref: FixedBytes([0x62; 32]),
        payer: addr(canister),
        tokenIn: addr(base_usdc),
        amountIn: U256::from(usdc(500)),
        deadline: U256::from(deadline_s),
        steps: vec![Step {
            target: addr(KYBER_ROUTER),
            data: sample_hex(&scaler_sample()["route_calldata"]).into(),
            token: addr(base_usdc),
            min: U256::ZERO,
            writes: vec![],
        }],
        payout: Payout {
            token: "0x4200000000000000000000000000000000000006"
                .parse()
                .unwrap(),
            recipient: addr(to),
            minOut: U256::from(200_000_000_000_000_000_u64),
        },
    })
}

/// 502.202203 ckUSDC pays exactly 501.65 USDC: less 10 bps (502,203) and 5 cents.
const PAYS_501_65: u64 = 502_202_203;

impl World {
    /// A fresh Base reading: a transaction is priced from it.
    fn push_reading(&self) {
        crate::client::settlement::push_chain_data(
            &self.pic,
            self.canister,
            crate::settlement_suite::init::watcher(),
            BASE,
            &settlement_api::types::chain_data::ChainData {
                block: 19_000_000,
                base_fee_wei_per_gas: Nat::from(1_000_000_000_u64),
                priority_fee_wei_per_gas: Nat::from(100_000_000_u64),
            },
        )
        .expect("the watcher may push");
    }

    /// The next outcall whose calls are exactly `methods`, after the rounds it takes.
    fn next_outcall(&self, methods: &[&str]) -> pocket_ic::common::rest::CanisterHttpRequest {
        for _ in 0..100 {
            let found = self.pic.get_canister_http().into_iter().find(|request| {
                let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                let asked: Vec<&str> = body
                    .as_array()
                    .map(|calls| {
                        calls
                            .iter()
                            .filter_map(|call| call["method"].as_str())
                            .collect()
                    })
                    .unwrap_or_default();
                asked == methods
            });
            if let Some(request) = found {
                return request;
            }
            self.pic.tick();
        }
        panic!("no outcall of {methods:?}");
    }

    /// Answers `request` with `result` for its one call: from one node, or from every node of
    /// the subnet alike for a read every node makes (`agreed`).
    fn reply(
        &self,
        request: &pocket_ic::common::rest::CanisterHttpRequest,
        result: serde_json::Value,
        agreed: bool,
    ) {
        use pocket_ic::common::rest::{
            CanisterHttpReply, CanisterHttpResponse, MockCanisterHttpResponse,
        };
        let body = serde_json::json!([{"jsonrpc": "2.0", "id": 0, "result": result}])
            .to_string()
            .into_bytes();
        let reply = || {
            CanisterHttpResponse::CanisterHttpReply(CanisterHttpReply {
                status: 200,
                headers: vec![],
                body: body.clone(),
            })
        };
        let subnet = self.pic.get_subnet(self.canister).expect("a subnet");
        let nodes = self.pic.topology().subnet_configs[&subnet].node_ids.len();
        let others = if agreed {
            (1..nodes).map(|_| reply()).collect()
        } else {
            vec![]
        };
        self.pic
            .mock_canister_http_response(MockCanisterHttpResponse {
                subnet_id: request.subnet_id,
                request_id: request.request_id,
                response: reply(),
                additional_responses: others,
            });
        for _ in 0..12 {
            self.pic.tick();
        }
    }
}

/// The payout plan (2026-10-23): an ICP user swaps 502.202203 ckUSDC to WETH on Base with
/// the quoter's plan for 500 USDC through KyberSwap. The canister pulls it, knows the exact
/// payout (501.65 USDC, the premium taken from what it pulled), reads KyberSwap's scaler for
/// exactly that, and signs the rewritten plan itself in the same call: no `pay_payout`.
#[test]
fn an_icp_user_swaps_ckusdc_with_a_plan_that_fits_and_it_is_signed_without_pay_payout() {
    use alloy_primitives::U256;
    use settlement_api::types::pool::{PayoutStatus, PayoutView, SwapFromIcpOk};
    let (world, canister_address) = swap_world();
    world.push_reading();
    world.approve(staker(), PAYS_501_65 + FEE);
    let deadline_s = world.now().as_secs().get() + 600;
    let terms = swap_terms(
        &world,
        PAYS_501_65,
        kyber_plan(canister_address, deadline_s),
        900_000,
    );
    let call = world
        .pic
        .submit_call(
            world.canister,
            staker(),
            "swap_from_icp",
            encode_one(terms).unwrap(),
        )
        .expect("submitted");
    // the scaler's read, once the pull answered: for exactly the payout
    let request = world.next_outcall(&["eth_call"]);
    let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
    let call_object = &body[0]["params"][0];
    assert_eq!(
        call_object["to"].as_str().unwrap().to_lowercase(),
        SCALER.to_lowercase()
    );
    let data = hex::decode(
        call_object["data"]
            .as_str()
            .unwrap()
            .trim_start_matches("0x"),
    )
    .unwrap();
    assert_eq!(data[..4], [0xe4, 0xda, 0x6f, 0x2e]);
    assert_eq!(
        data[36..68],
        U256::from(501_650_000_u64).to_be_bytes::<32>()
    );
    assert_eq!(
        world.balance(staker()),
        Nat::from(usdc(1_000) - PAYS_501_65 - 2 * FEE)
    );
    world.reply(
        &request,
        serde_json::json!(format!("0x{}", hex::encode(scaler_answer(501_650_000)))),
        false,
    );
    let raw = world.pic.await_call(call).expect("the swap answers");
    let swapped: Result<SwapFromIcpOk, PoolError> = candid::decode_one(&raw).unwrap();
    let swapped = swapped.expect("the swap is opened");
    let PayoutStatus::Signed(tx_id) = swapped.status else {
        panic!("signed at once, not {:?}", swapped.status);
    };
    // the order signed: exactly the payout in, its swap the scaler's own rewrite
    let pinned = world
        .pool_steps()
        .into_iter()
        .find_map(|step| match step {
            PoolStep::PayoutSigned { order, .. } => Some(order),
            _ => None,
        })
        .expect("the payout is signed");
    let order = types::swapic::decode_order(&pinned.order).unwrap();
    assert_eq!(order.amountIn, U256::from(501_650_000_u64));
    let scaled = types::swapic::scaled_route(&scaler_answer(501_650_000))
        .unwrap()
        .unwrap();
    assert_eq!(order.steps[0].data.to_vec(), scaled);
    let view: Option<PayoutView> = query(
        &world.pic,
        world.canister,
        world.admin,
        "payout",
        encode_one(swapped.id).unwrap(),
    );
    let view = view.expect("the payout is there");
    assert_eq!(view.status, PayoutStatus::Signed(tx_id));
    assert_eq!(view.amount, Some(Nat::from(501_650_000_u64)));
    assert_eq!(view.planned, Some(Nat::from(usdc(500))));
    assert_eq!(world.state().base.in_flight, Nat::from(501_650_000_u64));
    assert!(world.replay_matches());
}

/// A plan the scaler refuses to rewrite for the payout: the payout waits for the watcher's
/// order, as every payout did before the plan flow, listed for it.
#[test]
fn a_plan_the_scaler_refuses_waits_for_an_order() {
    use settlement_api::types::pool::{PayoutStatus, PayoutWaiting, SwapFromIcpOk};
    let (world, canister_address) = swap_world();
    world.push_reading();
    world.approve(staker(), PAYS_501_65 + FEE);
    let deadline_s = world.now().as_secs().get() + 600;
    let terms = swap_terms(
        &world,
        PAYS_501_65,
        kyber_plan(canister_address, deadline_s),
        900_000,
    );
    let call = world
        .pic
        .submit_call(
            world.canister,
            staker(),
            "swap_from_icp",
            encode_one(terms).unwrap(),
        )
        .expect("submitted");
    let request = world.next_outcall(&["eth_call"]);
    // the scaler's own refusal, as it answered 530 USDC on this route
    world.reply(
        &request,
        serde_json::json!(format!("0x{}", hex::encode(scaler_answer(530_000_000)))),
        false,
    );
    let raw = world.pic.await_call(call).expect("the swap answers");
    let swapped: Result<SwapFromIcpOk, PoolError> = candid::decode_one(&raw).unwrap();
    let swapped = swapped.expect("the swap is opened");
    assert_eq!(swapped.status, PayoutStatus::Waiting);
    let waiting: Vec<PayoutWaiting> = query(
        &world.pic,
        world.canister,
        crate::settlement_suite::init::watcher(),
        "payouts_waiting",
        encode_one(()).unwrap(),
    );
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].id, swapped.id);
    assert_eq!(waiting[0].amount, Nat::from(501_650_000_u64));
    assert!(world
        .pool_steps()
        .iter()
        .all(|step| !matches!(step, PoolStep::PayoutSigned { .. })));
}

/// Item 6: a payout whose transaction reverted is never signed again. Once its transaction
/// is out of the outbox and every node agrees Swapic never ran its order, the user's ckUSDC
/// comes back at once: all of it less one ledger fee, no Swapic fee.
#[test]
fn a_reverted_payout_refunds_its_ckusdc_once_final_and_unused() {
    use settlement_api::types::pool::{PayoutStatus, PayoutView, SwapFromIcpOk};
    let (world, canister_address) = swap_world();
    world.approve(staker(), usdc(100) + FEE);
    let payout = usdc(100) - 100_000 - 50_000;
    let deadline_s = world.now().as_secs().get() + 600;
    let plan = usdc_plan(canister_address, payout, deadline_s);
    // no chain reading: the plan fits but its transaction cannot be priced, so it waits
    let swapped: Result<SwapFromIcpOk, PoolError> = update(
        &world.pic,
        world.canister,
        staker(),
        "swap_from_icp",
        encode_one(swap_terms(&world, usdc(100), plan.clone(), 400_000)).unwrap(),
    );
    let swapped = swapped.expect("the swap is opened");
    assert_eq!(swapped.status, PayoutStatus::Waiting);
    let id = swapped.id;
    // its transaction signed, then reverted at depth, as the outbox records them
    let created = types::EventType::TxCreated {
        purpose: types::events::TxPurpose::PoolPayout(id),
        chain_id: ChainId::new(BASE),
        nonce: types::Nonce::new(0),
        to: base_chain().swapic.parse().unwrap(),
        value: types::Wei::ZERO,
        data: vec![0x01],
        gas_limit: types::GasAmount::from(400_000_u64),
        max_fee: types::WeiPerGas::new(2_000_000_000),
        max_priority_fee: types::WeiPerGas::new(100_000_000),
        action: Some(types::config::GasAction::PoolPayout),
    };
    append(
        &world.pic,
        world.canister,
        world.admin,
        &EventType::from(created),
    )
    .expect("the nonce is handed out");
    let order_id = [0x63; 32];
    world.plant(PoolStep::PayoutSigned {
        id,
        order: types::pool::PinnedOrder {
            order_id,
            order: plan,
            gas_limit: 400_000,
            deadline_s,
        },
        chain_id: ChainId::new(BASE),
        nonce: types::Nonce::new(0),
        tx_hash: TxHash::new([0x64; 32]),
        raw_tx: vec![0x02, 0x01],
    });
    world.plant(PoolStep::PayoutReverted { id });
    let before = world.balance(staker());
    // the payout timer: every node reads Swapic's `used` at one agreed block
    world.pic.advance_time(Duration::from_secs(61));
    let head = world.next_outcall(&["eth_blockNumber"]);
    world.reply(&head, serde_json::json!("0x1ec55b90"), true);
    let used = world.next_outcall(&["eth_call"]);
    let body: serde_json::Value = serde_json::from_slice(&used.body).unwrap();
    assert_ne!(body[0]["params"][1], "latest", "read at an agreed block");
    world.reply(
        &used,
        serde_json::json!(format!("0x{}", "00".repeat(32))),
        true,
    );
    world.settle();
    let view: Option<PayoutView> = query(
        &world.pic,
        world.canister,
        world.admin,
        "payout",
        encode_one(id).unwrap(),
    );
    assert_eq!(
        view.expect("the payout is there").status,
        PayoutStatus::Undone("its transaction reverted".to_string())
    );
    let refunded = before + Nat::from(usdc(100) - FEE);
    assert_eq!(world.balance(staker()), refunded);
    // undone once: nothing more moves
    world.next_tick();
    assert_eq!(world.balance(staker()), refunded);
    let _ = order_id;
    assert!(world.replay_matches());
}

/// `resolve_payout` (2026-10-23): a payout stopped for a human (its reverted order ran after
/// all, as every node read Swapic's `used`) held its lines for good. A controller refunds it:
/// the user's ckUSDC comes back at once, all of it less one ledger fee, once; a stranger may
/// not resolve it.
#[test]
fn a_stopped_payout_a_controller_refunds_pays_its_user_back_once() {
    use settlement_api::types::pool::{PayoutResolution, PayoutStatus, PayoutView, SwapFromIcpOk};
    let (world, canister_address) = swap_world();
    world.approve(staker(), usdc(100) + FEE);
    let payout = usdc(100) - 100_000 - 50_000;
    let deadline_s = world.now().as_secs().get() + 600;
    let plan = usdc_plan(canister_address, payout, deadline_s);
    let swapped: Result<SwapFromIcpOk, PoolError> = update(
        &world.pic,
        world.canister,
        staker(),
        "swap_from_icp",
        encode_one(swap_terms(&world, usdc(100), plan.clone(), 400_000)).unwrap(),
    );
    let id = swapped.expect("the swap is opened").id;
    let created = types::EventType::TxCreated {
        purpose: types::events::TxPurpose::PoolPayout(id),
        chain_id: ChainId::new(BASE),
        nonce: types::Nonce::new(0),
        to: base_chain().swapic.parse().unwrap(),
        value: types::Wei::ZERO,
        data: vec![0x01],
        gas_limit: types::GasAmount::from(400_000_u64),
        max_fee: types::WeiPerGas::new(2_000_000_000),
        max_priority_fee: types::WeiPerGas::new(100_000_000),
        action: Some(types::config::GasAction::PoolPayout),
    };
    append(
        &world.pic,
        world.canister,
        world.admin,
        &EventType::from(created),
    )
    .expect("the nonce is handed out");
    world.plant(PoolStep::PayoutSigned {
        id,
        order: types::pool::PinnedOrder {
            order_id: [0x65; 32],
            order: plan,
            gas_limit: 400_000,
            deadline_s,
        },
        chain_id: ChainId::new(BASE),
        nonce: types::Nonce::new(0),
        tx_hash: TxHash::new([0x66; 32]),
        raw_tx: vec![0x02, 0x01],
    });
    world.plant(PoolStep::PayoutReverted { id });
    world.pic.advance_time(Duration::from_secs(61));
    let head = world.next_outcall(&["eth_blockNumber"]);
    world.reply(&head, serde_json::json!("0x1ec55b90"), true);
    let used = world.next_outcall(&["eth_call"]);
    // Swapic says the order ran after all: a human's
    world.reply(
        &used,
        serde_json::json!(format!("0x{}01", "00".repeat(31))),
        true,
    );
    world.settle();
    let status = |world: &World| {
        let view: Option<PayoutView> = query(
            &world.pic,
            world.canister,
            world.admin,
            "payout",
            encode_one(id).unwrap(),
        );
        view.expect("the payout is there").status
    };
    assert!(matches!(status(&world), PayoutStatus::Failed(_)));
    let before = world.balance(staker());
    let resolve = |sender: Principal| -> Result<(), PoolError> {
        update(
            &world.pic,
            world.canister,
            sender,
            "resolve_payout",
            candid::encode_args((id, PayoutResolution::Refund)).unwrap(),
        )
    };
    assert!(matches!(resolve(staker()), Err(PoolError::NotAllowed(_))));
    assert_eq!(resolve(world.admin), Ok(()));
    world.settle();
    assert_eq!(
        status(&world),
        PayoutStatus::Undone("refunded by a controller".to_string())
    );
    let refunded = before + Nat::from(usdc(100) - FEE);
    assert_eq!(world.balance(staker()), refunded);
    assert!(matches!(
        resolve(world.admin),
        Err(PoolError::NotAllowed(_))
    ));
    world.next_tick();
    assert_eq!(world.balance(staker()), refunded);
    assert!(world.replay_matches());
}

/// The per-caller limit (Jeff, 2026-10-23): a caller with no approval calls the free doors
/// over and over. Each call let through writes its lines and is refused its pull; past five a
/// minute, both doors together, it is refused with nothing written. Another caller is
/// counted apart.
#[test]
fn a_caller_past_five_calls_a_minute_is_refused_with_nothing_written() {
    use settlement_api::types::pool::{StakeIcpArgs, StakeIcpOk, SwapFromIcpOk};
    let (world, canister_address) = swap_world();
    let deadline_s = world.now().as_secs().get() + 600;
    let plan = usdc_plan(canister_address, usdc(100) - 150_000, deadline_s);
    let swap = |caller: Principal| -> Result<SwapFromIcpOk, PoolError> {
        update(
            &world.pic,
            world.canister,
            caller,
            "swap_from_icp",
            encode_one(swap_terms(&world, usdc(100), plan.clone(), 400_000)).unwrap(),
        )
    };
    for _ in 0..4 {
        assert!(matches!(swap(staker()), Err(PoolError::Ledger(_))));
    }
    // the stake door counts with it
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
    assert!(staked.is_err());
    let count = crate::client::settlement::event_count(&world.pic, world.canister, world.admin);
    assert!(matches!(
        swap(staker()),
        Err(PoolError::Unavailable(why)) if why.contains("a minute")
    ));
    assert_eq!(
        crate::client::settlement::event_count(&world.pic, world.canister, world.admin),
        count,
        "nothing written"
    );
    // another caller is counted apart
    assert!(matches!(swap(recipient()), Err(PoolError::Ledger(_))));
    // a minute on, the first passes again
    world.pic.advance_time(Duration::from_secs(61));
    world.books_checked();
    assert!(matches!(swap(staker()), Err(PoolError::Ledger(_))));
}

/// M-03: a pool arrival's receipt decides a credit alone, so every node reads it and they
/// must agree. One node reads the head, which only names the block every node then reads
/// beside the receipt. A receipt one node's provider makes up is outvoted by the others',
/// and nodes split on it agree on nothing: either way nothing is credited, and the report is
/// asked again.
#[test]
fn a_pool_arrivals_receipt_is_read_by_every_node() {
    use pocket_ic::common::rest::{
        CanisterHttpReply, CanisterHttpResponse, MockCanisterHttpResponse,
    };
    use settlement_api::types::pool::{
        PoolArrival, PoolArrivalOutcome, PoolDepositKind, PoolTerms,
    };
    let world = signing_world(
        usdc(1_000),
        Config {
            pool_chain: Some(BASE),
            evm_chains: std::collections::BTreeMap::from([(BASE, base_chain())]),
            rpc_urls: std::collections::BTreeMap::from([(
                BASE,
                "https://base-mainnet.example/v2/key".to_string(),
            )]),
            ..init_arg().config
        },
    );
    crate::client::settlement::derive_evm_address(&world.pic, world.canister, world.admin)
        .expect("the test key derives an address");
    let subnet = world.pic.get_subnet(world.canister).expect("a subnet");
    let nodes = world.pic.topology().subnet_configs[&subnet].node_ids.len();
    let answer = |receipt: serde_json::Value| {
        CanisterHttpResponse::CanisterHttpReply(CanisterHttpReply {
            status: 200,
            headers: vec![],
            body: serde_json::json!([
                {"jsonrpc": "2.0", "id": 0, "result": receipt},
                {"jsonrpc": "2.0", "id": 1, "result": {
                    "number": "0x100", "hash": format!("0x{}", "cd".repeat(32)), "timestamp": "0x6ad5e897",
                }},
            ])
            .to_string()
            .into_bytes(),
        })
    };
    let made_up = serde_json::json!({
        "transactionHash": format!("0x{}", "ab".repeat(32)),
        "blockNumber": "0xf0",
        "status": "0x1",
        "logs": [],
    });
    // node `i` answers the made-up receipt when `lies(i)`, else no receipt
    let report = |lies: &dyn Fn(usize) -> bool| -> Result<PoolArrivalOutcome, PoolError> {
        let arrival = PoolArrival {
            base_tx: vec![0xab; 32],
            src_chain: BASE,
            src_tx: vec![],
            terms: PoolTerms {
                kind: PoolDepositKind::Stake {
                    owner: PoolAccount::Evm(
                        "0x1111111111111111111111111111111111111111".to_string(),
                    ),
                },
                premium: Nat::from(0_u32),
                icp_fees: Nat::from(0_u32),
                rebalance_share: Nat::from(0_u32),
                salt: vec![7; 32],
            },
        };
        let call = world
            .pic
            .submit_call(
                world.canister,
                world.admin,
                "note_pool_arrival",
                encode_one(arrival).unwrap(),
            )
            .expect("the door takes the call");
        let head = world.next_outcall(&["eth_blockNumber"]);
        world.reply(&head, serde_json::json!("0x100"), false);
        let read = world.next_outcall(&["eth_getTransactionReceipt", "eth_getBlockByNumber"]);
        let body: serde_json::Value = serde_json::from_slice(&read.body).unwrap();
        assert_eq!(
            body[0]["params"],
            serde_json::json!([format!("0x{}", "ab".repeat(32))])
        );
        assert_eq!(body[1]["params"], serde_json::json!(["0x100", false]));
        let mut answers = (0..nodes).map(|node| {
            answer(if lies(node) {
                made_up.clone()
            } else {
                serde_json::Value::Null
            })
        });
        world
            .pic
            .mock_canister_http_response(MockCanisterHttpResponse {
                subnet_id: read.subnet_id,
                request_id: read.request_id,
                response: answers.next().unwrap(),
                additional_responses: answers.collect(),
            });
        candid::decode_one(&world.pic.await_call(call).expect("the door answers")).unwrap()
    };
    let outvoted = report(&|node| node == 0);
    assert!(
        matches!(&outvoted, Err(PoolError::Unavailable(why)) if why.contains("no receipt")),
        "{outvoted:?}"
    );
    let split = report(&|node| node % 2 == 0);
    assert!(
        matches!(&split, Err(PoolError::Unavailable(why)) if why.contains("Unreachable")),
        "{split:?}"
    );
}
