use super::*;
use crate::storage::events::{event_at, event_count, verify_replay};
use crate::storage::on_fresh_memory;
use candid::Principal;
use types::books::{BalanceCheck, BookStep};
use types::events::TxPurpose;
use types::hash::{PoolOpId, TxHash};
use types::pool::{
    DepositKind, DepositStatus, IcpAccount, LedgerFee, PoolRail, PoolSettings, Registration,
    CKUSDC_LEDGER,
};
use types::{BlockNumber, ChainId, TokenAmount};

pub(crate) const FEE: u64 = 10_000;

pub(crate) fn usdc(whole: u64) -> TokenAmount {
    TokenAmount::from(whole * 1_000_000)
}

pub(crate) fn ckusdc() -> Principal {
    Principal::from_text(CKUSDC_LEDGER).unwrap()
}

pub(crate) fn at(nanos: u64) -> Timestamp {
    Timestamp::from_nanos(nanos)
}

pub(crate) fn settings() -> PoolSettings {
    PoolSettings {
        open_for_swaps: true,
        ledger_fees: vec![LedgerFee {
            ledger: ckusdc(),
            fee: TokenAmount::from(FEE),
        }],
        ..PoolSettings::default()
    }
}

pub(crate) fn account(byte: u8) -> IcpAccount {
    IcpAccount {
        owner: Principal::from_slice(&[byte; 10]),
        subaccount: None,
    }
}

pub(crate) fn payer() -> types::EvmAddress {
    "0xf00EBA8E5Fd231b526ADFAAa68B4d0ede72A89AC"
        .parse()
        .unwrap()
}

/// A ckUSDC swap for `account(byte)` registered to expire at `expires_at`.
pub(crate) fn swap_terms(
    byte: u8,
    min_usdc: TokenAmount,
    premium: TokenAmount,
    expires_at: Timestamp,
) -> Registration {
    Registration {
        kind: DepositKind::Swap {
            recipient: account(byte),
            token_out: ckusdc(),
            min_out: 1,
        },
        min_usdc,
        premium,
        move_fee: TokenAmount::ZERO,
        src_chain: ChainId::ARBITRUM,
        src_payer: payer(),
        rail: PoolRail::Cctp,
        expires_at,
        salt: [byte; 32],
        icp_fees: Some(TokenAmount::from(FEE)),
        least_usdc: TokenAmount::ZERO,
    }
}

/// Settings, and `icp` of ckUSDC staked on ICP by `account(200)`, recorded at `at(1)`.
pub(crate) fn funded_pool(icp: TokenAmount) {
    record_at(
        PoolStep::SettingsSet {
            settings: settings(),
        },
        at(1),
    )
    .unwrap();
    let id = PoolOpId::new([0xee; 32]);
    let owner = PoolAccount::Icp(account(200));
    record_at(
        PoolStep::StakeStarted {
            id,
            owner,
            from: account(200),
            amount: icp,
            created_at: at(1),
            move_fee: TokenAmount::ZERO,
        },
        at(1),
    )
    .unwrap();
    let shares = ledger().mint_now(icp).unwrap();
    record_at(
        PoolStep::StakedIcp {
            id,
            owner,
            amount: icp,
            move_fee: TokenAmount::ZERO,
            shares,
            block: 1,
        },
        at(1),
    )
    .unwrap();
}

/// Registers `terms`, answering its ref.
pub(crate) fn register(terms: Registration) -> PoolRef {
    let r#ref = terms.pool_ref().unwrap();
    record_at(
        PoolStep::Registered {
            r#ref,
            deposit: terms,
        },
        at(2),
    )
    .unwrap();
    r#ref
}

pub(crate) fn credit(log: u64) -> CreditKey {
    CreditKey {
        chain: ChainId::BASE,
        tx: TxHash::new([0xab; 32]),
        log_index: log,
    }
}

#[test]
fn a_recorded_step_is_in_the_log_and_the_fold() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        let r#ref = register(swap_terms(1, usdc(100), usdc(1), at(1_000)));
        let count = event_count();
        assert!(matches!(
            event_at(count - 1).unwrap().payload,
            EventType::Pool {
                step: PoolStep::Registered { .. }
            }
        ));
        assert_eq!(deposit(&r#ref).unwrap().status, DepositStatus::Registered);
        assert_eq!(
            ledger().pockets.icp.reserved,
            TokenAmount::ZERO,
            "a registration holds nothing"
        );
        assert_eq!(
            shares_of(&PoolAccount::Icp(account(200))),
            ledger().total_shares
        );
        record_at(
            PoolStep::Arrived {
                r#ref,
                credit: credit(4),
                amount: usdc(100),
                block: BlockNumber::new(50),
            },
            at(3),
        )
        .unwrap();
        assert!(credited(&credit(4)));
        assert!(!credited(&credit(5)));
    });
}

#[test]
fn a_refused_step_appends_nothing() {
    on_fresh_memory(|| {
        crate::storage::init();
        // no settings: nothing registers
        let terms = swap_terms(1, usdc(100), usdc(1), at(1_000));
        let refused = record_at(
            PoolStep::Registered {
                r#ref: terms.pool_ref().unwrap(),
                deposit: terms,
            },
            at(1),
        );
        assert!(matches!(
            refused,
            Err(AppendError::Transition(types::TransitionError::Pool(
                types::pool::PoolStepError::NoSettings
            )))
        ));
        assert_eq!(event_count(), 0);
        funded_pool(usdc(10));
        let count = event_count();
        // a ref its terms do not commit to
        let terms = swap_terms(1, usdc(100), usdc(1), at(1_000));
        assert!(record_at(
            PoolStep::Registered {
                r#ref: types::hash::PoolRef::new([0x0b; 32]),
                deposit: terms,
            },
            at(2),
        )
        .is_err());
        assert_eq!(event_count(), count);
        assert_eq!(ledger().pockets.icp.reserved, TokenAmount::ZERO);
    });
}

/// The deep audit folds the log onto the heap: with pool lines in it, the heap fold is the
/// stable fold, the pool's ledger, deposits, shares and credited transfers included.
#[test]
fn the_replay_of_a_log_with_pool_lines_matches_the_live_state() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        let paid = register(swap_terms(1, usdc(100), usdc(1), at(1_000)));
        let expired = register(swap_terms(2, usdc(50), usdc(1), at(1_000)));
        record_at(
            PoolStep::Arrived {
                r#ref: paid,
                credit: credit(1),
                amount: usdc(100),
                block: BlockNumber::new(50),
            },
            at(3),
        )
        .unwrap();
        record_at(PoolStep::Expired { r#ref: expired }, at(4)).unwrap();
        crate::storage::events::append_event_at(
            EventType::Books {
                chain_id: ChainId::BASE,
                step: BookStep::Checked {
                    check: BalanceCheck {
                        block: BlockNumber::new(50),
                        observed: usdc(100),
                        expected: usdc(100),
                    },
                    shortfall: false,
                },
            },
            at(5),
        )
        .unwrap();
        assert!(verify_replay(), "the heap fold of the log is the live fold");
        // the check read the arrival's block, so the arrival is in the balance it read
        assert!(ledger().unseen.is_empty());
    });
}

/// A controller's reopening of the pool clears the ICP pocket's latch with the Base one.
#[test]
fn reopening_the_pool_clears_the_icp_latch() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1));
        record_at(
            PoolStep::IcpChecked {
                check: types::pool::IcpCheck {
                    observed: TokenAmount::ZERO,
                    expected: usdc(1),
                    shortfall: true,
                },
            },
            at(7),
        )
        .unwrap();
        assert_eq!(ledger().icp_shortfall_at, Some(at(7)));
        crate::storage::events::append_event_at(
            EventType::Books {
                chain_id: ChainId::BASE,
                step: BookStep::PoolPaused { paused: false },
            },
            at(8),
        )
        .unwrap();
        assert_eq!(ledger().icp_shortfall_at, None);
        assert!(verify_replay());
    });
}

/// `usdc` staked on Base by the EVM `staker`, through a registration that arrived.
pub(crate) fn base_stake(staker: types::EvmAddress, amount: TokenAmount, salt: u8, log: u64) {
    let terms = Registration {
        kind: DepositKind::Stake {
            owner: PoolAccount::Evm(staker),
        },
        min_usdc: amount,
        premium: TokenAmount::ZERO,
        move_fee: TokenAmount::ZERO,
        src_chain: ChainId::BASE,
        src_payer: payer(),
        rail: PoolRail::SameChain,
        expires_at: at(1_000),
        salt: [salt; 32],
        icp_fees: None,
        least_usdc: TokenAmount::ZERO,
    };
    let r#ref = register(terms);
    record_at(
        PoolStep::Arrived {
            r#ref,
            credit: credit(log),
            amount,
            block: BlockNumber::new(40),
        },
        at(3),
    )
    .unwrap();
    let shares = ledger().mint_now(amount).unwrap();
    record_at(
        PoolStep::Minted {
            r#ref,
            owner: PoolAccount::Evm(staker),
            shares,
        },
        at(3),
    )
    .unwrap();
}

#[test]
fn the_replay_of_a_log_with_payout_lines_matches_the_live_state() {
    use types::pool::{
        EvmWithdrawal, PayoutKind, PayoutSettings, PayoutStatus, PayoutTerms, PinnedOrder, Shares,
        Undo,
    };
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        let staker = payer();
        base_stake(staker, usdc(2_000), 0x51, 1);
        record_at(
            PoolStep::PayoutSettingsSet {
                settings: PayoutSettings::default(),
            },
            at(4),
        )
        .unwrap();
        let pool = ledger();
        let burned = Shares::burn_for(usdc(500), pool.total_assets(), pool.total_shares).unwrap();
        let withdrawal = EvmWithdrawal {
            staker,
            usdc: usdc(500),
            max_shares: burned,
            move_fee: TokenAmount::from(1_000_u32),
            network_fee: TokenAmount::from(50_000_u32),
            nonce: 1,
            burned,
        };
        let terms = PayoutTerms {
            dst_chain: ChainId::BASE,
            recipient: staker,
            token_out: staker,
            min_out: TokenAmount::from(1_u8),
            deadline: at(1_000_000),
        };
        let steps = [
            PoolStep::PayoutOpened {
                id: 0,
                kind: Box::new(PayoutKind::Withdrawal(withdrawal)),
                terms,
                undo: Undo::Remint {
                    owner: PoolAccount::Evm(staker),
                    shares: burned,
                },
                plan: None,
                ck: Default::default(),
            },
            PoolStep::PayoutAmountKnown {
                id: 0,
                amount: withdrawal.payout_amount().unwrap(),
                received: None,
            },
            PoolStep::PayoutWaiting { id: 0 },
        ];
        for step in steps {
            record_at(step, at(5)).unwrap();
        }
        // the payout's transaction is created first, its nonce held for it
        crate::storage::events::append_event_at(created(TxPurpose::PoolPayout(0), 0), at(6))
            .unwrap();
        let signed = record_at(
            PoolStep::PayoutSigned {
                id: 0,
                order: PinnedOrder {
                    order_id: [1; 32],
                    order: vec![1; 8],
                    gas_limit: 300_000,
                    deadline_s: 1_000,
                },
                chain_id: ChainId::BASE,
                nonce: types::Nonce::new(0),
                tx_hash: TxHash::new([2; 32]),
                raw_tx: vec![2; 8],
            },
            at(6),
        )
        .unwrap();
        let payout = read_state(|state| state.store().pool_payout(0)).unwrap();
        // the payout is known by the line that recorded its transaction
        assert_eq!(payout.status, PayoutStatus::Signed(signed.get()));
        assert!(read_state(|state| state.store().is_withdrawal_nonce_used(
            &types::pool::NonceUse { staker, nonce: 1 }
        )));
        assert!(verify_replay(), "the heap fold of the log is the live fold");
    });
}

/// A `TxCreated` on Base at `nonce` for `purpose`.
pub(crate) fn created(purpose: TxPurpose, nonce: u64) -> EventType {
    EventType::TxCreated {
        purpose,
        chain_id: ChainId::BASE,
        nonce: types::Nonce::new(nonce),
        to: payer(),
        value: types::Wei::ZERO,
        data: vec![1, 2, 3],
        gas_limit: types::GasAmount::from(300_000_u64),
        max_fee: types::WeiPerGas::new(1_000),
        max_priority_fee: types::WeiPerGas::new(10),
        action: Some(types::config::GasAction::PoolApprove),
    }
}

/// A pool transaction's signed record spends exactly the nonce handed out for its purpose:
/// never one handed out for another, never one not handed out; and it is indexed among the
/// signed transactions like any other.
#[test]
fn a_pool_signature_spends_the_nonce_held_for_its_purpose() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        let approve = |nonce: u64| PoolStep::ApproveSigned {
            chain_id: ChainId::BASE,
            nonce: types::Nonce::new(nonce),
            tx_hash: TxHash::new([7; 32]),
            raw_tx: vec![7; 4],
        };
        // no nonce handed out: refused
        assert!(record_at(approve(0), at(5)).is_err());
        // a nonce handed out for another purpose: refused
        crate::storage::events::append_event_at(created(TxPurpose::PoolPayout(9), 0), at(5))
            .unwrap_err();
        crate::storage::events::append_event_at(created(TxPurpose::PoolApprove, 0), at(5)).unwrap();
        // one approval holds one nonce at a time
        assert!(
            crate::storage::events::append_event_at(created(TxPurpose::PoolApprove, 1), at(5))
                .is_err()
        );
        assert!(record_at(approve(1), at(6)).is_err());
        let signed = record_at(approve(0), at(6)).unwrap();
        assert!(read_state(|state| state.unsigned_nonces()).is_empty());
        assert_eq!(
            read_state(|state| state.store().signed_in(signed.., 10)),
            vec![signed]
        );
        // spent: a second record of it is refused
        assert!(record_at(approve(0), at(7)).is_err());
        assert!(verify_replay(), "the heap fold of the log is the live fold");
    });
}

/// A pool with 1,000 ckUSDC on ICP, the payout settings, and `staker` with 500 USDC staked
/// on Base, whose withdrawal of 100 USDC waits for its order: payout 0.
pub(crate) fn waiting_withdrawal(staker: types::EvmAddress) -> u64 {
    use types::pool::{EvmWithdrawal, PayoutKind, PayoutSettings, PayoutTerms, Shares, Undo};
    funded_pool(usdc(1_000));
    record_at(
        PoolStep::PayoutSettingsSet {
            settings: PayoutSettings::default(),
        },
        at(1),
    )
    .unwrap();
    base_stake(staker, usdc(500), 0x77, 1);
    let pool = ledger();
    let burned = Shares::burn_for(usdc(100), pool.total_assets(), pool.total_shares).unwrap();
    let withdrawal = EvmWithdrawal {
        staker,
        usdc: usdc(100),
        max_shares: burned,
        move_fee: TokenAmount::from(20_000_u64),
        network_fee: TokenAmount::from(50_000_u64),
        nonce: 1,
        burned,
    };
    let id = pool.lines().next_id;
    for step in [
        PoolStep::PayoutOpened {
            id,
            kind: Box::new(PayoutKind::Withdrawal(withdrawal)),
            terms: PayoutTerms {
                dst_chain: ChainId::BASE,
                recipient: staker,
                token_out: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"
                    .parse()
                    .unwrap(),
                min_out: TokenAmount::ONE,
                deadline: at(1_000_000_000_000),
            },
            undo: Undo::Remint {
                owner: PoolAccount::Evm(staker),
                shares: burned,
            },
            plan: None,
            ck: Default::default(),
        },
        PoolStep::PayoutAmountKnown {
            id,
            amount: withdrawal.payout_amount().unwrap(),
            received: None,
        },
        PoolStep::PayoutWaiting { id },
    ] {
        record_at(step, at(2)).unwrap();
    }
    id
}

/// The payout settings as they stand with an Across in-flight cap of a million USDC: what
/// an Across payout is signed under (D16).
pub(crate) fn across_cap() {
    let settings = ledger().lines().settings.unwrap_or_default();
    record_at(
        PoolStep::PayoutSettingsSet {
            settings: types::pool::PayoutSettings {
                across_in_flight_max: Some(usdc(1_000_000)),
                ..settings
            },
        },
        at(2),
    )
    .unwrap();
}

/// Payout `id` signed at `nonce` on Base with order `byte`: its creation and its record.
pub(crate) fn sign_payout(id: u64, nonce: u64, byte: u8) -> types::EventIndex {
    sign_payout_with(id, nonce, byte, vec![byte; 8])
}

/// [`sign_payout`] with `order` as the order's bytes.
pub(crate) fn sign_payout_with(id: u64, nonce: u64, byte: u8, order: Vec<u8>) -> types::EventIndex {
    crate::storage::events::append_event_at(created(TxPurpose::PoolPayout(id), nonce), at(3))
        .unwrap();
    record_at(
        PoolStep::PayoutSigned {
            id,
            order: types::pool::PinnedOrder {
                order_id: [byte; 32],
                order,
                gas_limit: 300_000,
                deadline_s: 900,
            },
            chain_id: ChainId::BASE,
            nonce: types::Nonce::new(nonce),
            tx_hash: TxHash::new([byte; 32]),
            raw_tx: vec![byte; 4],
        },
        at(3),
    )
    .unwrap()
}

/// An order publishing one Eco intent, a USDC delivery on Ethereum, whose route can last be
/// filled at `route_deadline`: what a pinned order reads an intent's deadline from.
pub(crate) fn eco_order(route_deadline: u64) -> Vec<u8> {
    use alloy_primitives::{Address, FixedBytes, U256};
    use alloy_sol_types::SolCall;
    use types::swapic::{publishAndFundCall, EcoReward, EcoTokenAmount, Order, Payout, Step};
    let usdc = Address::repeat_byte(0x11);
    let route = types::swapic::deliver_route(
        [0x5a; 32],
        route_deadline,
        types::EvmAddress::new([0xec; 20]),
        types::EvmAddress::new([0x11; 20]),
        types::EvmAddress::new([0x36; 20]),
        U256::ZERO,
    );
    let publish = publishAndFundCall {
        destination: 1,
        route: route.into(),
        reward: EcoReward {
            deadline: route_deadline + 7 * 24 * 3_600,
            creator: Address::repeat_byte(0x36),
            prover: Address::repeat_byte(0xec),
            nativeAmount: U256::ZERO,
            tokens: vec![EcoTokenAmount {
                token: usdc,
                amount: U256::ZERO,
            }],
        },
        allowPartial: false,
    };
    types::swapic::encode_order(&Order {
        r#ref: FixedBytes([0x85; 32]),
        payer: Address::repeat_byte(0x36),
        tokenIn: usdc,
        amountIn: U256::from(1_000_000_000_u64),
        deadline: U256::from(route_deadline.saturating_sub(600)),
        steps: vec![Step {
            target: Address::repeat_byte(0xec),
            data: publish.abi_encode().into(),
            token: usdc,
            min: U256::ZERO,
            writes: vec![],
        }],
        payout: Payout {
            token: Address::ZERO,
            recipient: Address::ZERO,
            minOut: U256::ZERO,
        },
        memo: Default::default(),
    })
}

/// A signature the fold refuses for the Across in-flight cap never exists for anyone (review
/// of 5a725a7): the signed record is checked before it is written (`append_event_at`:
/// `check_at`, then the append), so nothing is logged, nothing is in `signed_txs`, and the
/// outbox, which `tx::create_and_send_with` fills only after the record is written, never
/// holds it. The nonce stays handed out and unsigned, for the stranded-nonce pass to cancel;
/// the payout waits on.
#[test]
fn a_payout_signature_refused_for_the_across_cap_is_never_written() {
    use alloy_primitives::{Address, U256};
    use types::pool::{PayoutSettings, PayoutStatus};
    use types::swapic::{Order, Payout, Step};
    on_fresh_memory(|| {
        crate::storage::init();
        let id = waiting_withdrawal("0x1111111111111111111111111111111111111111".parse().unwrap());
        // a cap of one USDC, under the payout's
        let settings = ledger().lines().settings.unwrap_or_default();
        record_at(
            PoolStep::PayoutSettingsSet {
                settings: PayoutSettings {
                    across_in_flight_max: Some(usdc(1)),
                    ..settings
                },
            },
            at(2),
        )
        .unwrap();
        let mut data = types::swapic::across::SWAP_AND_BRIDGE.to_vec();
        data.extend_from_slice(&[0x5d; 32]);
        let order = types::swapic::encode_order(&Order {
            r#ref: [0x5d; 32].into(),
            payer: Address::repeat_byte(0x8f),
            tokenIn: Address::repeat_byte(0x83),
            amountIn: U256::from(1u8),
            deadline: U256::from(900u64),
            steps: vec![Step {
                target: Address::repeat_byte(0x97),
                data: data.into(),
                token: Address::repeat_byte(0x83),
                min: U256::from(1u8),
                writes: Vec::new(),
            }],
            payout: Payout {
                token: Address::ZERO,
                recipient: Address::ZERO,
                minOut: U256::ZERO,
            },
            memo: Vec::new().into(),
        });
        crate::storage::events::append_event_at(created(TxPurpose::PoolPayout(id), 0), at(3))
            .unwrap();
        let before = event_count();
        let refused = record_at(
            PoolStep::PayoutSigned {
                id,
                order: types::pool::PinnedOrder {
                    order_id: [0x5d; 32],
                    order,
                    gas_limit: 300_000,
                    deadline_s: 900,
                },
                chain_id: ChainId::BASE,
                nonce: types::Nonce::new(0),
                tx_hash: TxHash::new([0x5d; 32]),
                raw_tx: vec![0x5d; 4],
            },
            at(3),
        );
        assert!(refused.is_err());
        assert_eq!(event_count(), before);
        let (signed, unsigned) = crate::storage::events::read_state(|state| {
            (
                state.store().signed_in(types::EventIndex::new(0).., 10),
                state.store().unsigned_nonce(&types::NonceKey {
                    chain_id: ChainId::BASE,
                    nonce: types::Nonce::new(0),
                }),
            )
        });
        assert!(signed.is_empty());
        assert!(unsigned.is_some());
        assert_eq!(payout(id).unwrap().status, PayoutStatus::Waiting);
        assert!(payout(id).unwrap().order.is_none());
    });
}
