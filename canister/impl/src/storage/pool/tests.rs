use super::*;
use crate::storage::events::{event_at, event_count, verify_replay};
use crate::storage::on_fresh_memory;
use candid::Principal;
use types::books::{BalanceCheck, BookStep};
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
        rebalance_share: TokenAmount::ZERO,
        src_chain: ChainId::ARBITRUM,
        src_payer: payer(),
        rail: PoolRail::Cctp,
        expires_at,
        salt: [byte; 32],
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
            rebalance_share: TokenAmount::ZERO,
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
        assert_eq!(waiting_refs(at(500)), vec![r#ref]);
        // past its expiry and grace it is still read for one read margin (review fix 13):
        // an arrival on time may not have been read yet; past that it waits no more
        let grace = settings().deposit_grace.as_nanos() as u64;
        let margin = read_state(|state| read_margin_in(state.store())).as_nanos() as u64;
        assert_eq!(
            read_margin(std::time::Duration::from_secs(300), 3),
            std::time::Duration::from_secs(306)
        );
        assert_eq!(waiting_refs(at(1_000 + grace + 1)), vec![r#ref]);
        assert_eq!(waiting_refs(at(1_000 + grace + margin)), vec![r#ref]);
        assert!(waiting_refs(at(1_000 + grace + margin + 1)).is_empty());
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
        assert!(waiting_refs(at(500)).is_empty());
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
        rebalance_share: TokenAmount::ZERO,
        src_chain: ChainId::BASE,
        src_payer: payer(),
        rail: PoolRail::SameChain,
        expires_at: at(1_000),
        salt: [salt; 32],
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
            rebalance_share: TokenAmount::from(1_000_u32),
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
