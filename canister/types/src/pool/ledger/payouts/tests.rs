use super::super::tests::{credit, funded, payer, recipient, register, usdc, Pool};
use crate::chain::ChainId;
use crate::evm::EvmAddress;
use crate::hash::TxHash;
use crate::numeric::{BlockNumber, Nonce, Timestamp, TokenAmount};
use crate::pool::payout::{
    EvmWithdrawal, IcpSwapTerms, PayoutKind, PayoutSettings, PayoutStatus, PayoutTerms,
    PinnedOrder, Undo,
};
use crate::pool::{
    DepositKind, PoolAccount, PoolRail, PoolStep, PoolStepError, Registration, Shares,
};

fn staker() -> EvmAddress {
    "0x1111111111111111111111111111111111111111"
        .parse()
        .unwrap()
}

fn terms() -> PayoutTerms {
    PayoutTerms {
        dst_chain: ChainId::ARBITRUM,
        recipient: payer(),
        token_out: "0xaf88d065e77c8cC2239327C5EDb3A432268e5831"
            .parse()
            .unwrap(),
        min_out: TokenAmount::from(1_u8),
        deadline: Timestamp::from_nanos(1_800_000_000_000_000_000),
    }
}

/// `pool` with the payout settings and `base` of USDC staked on Base by `staker()`.
fn with_base(mut pool: Pool, base: TokenAmount, salt: u8, log: u64) -> Pool {
    if pool.ledger.settings.is_none() {
        pool.step(PoolStep::SettingsSet {
            settings: super::super::tests::settings(),
        })
        .unwrap();
    }
    if pool.ledger.lines().settings.is_none() {
        pool.step(PoolStep::PayoutSettingsSet {
            settings: PayoutSettings::default(),
        })
        .unwrap();
    }
    let terms = Registration {
        kind: DepositKind::Stake {
            owner: PoolAccount::Evm(staker()),
        },
        min_usdc: base,
        premium: TokenAmount::ZERO,
        rebalance_share: TokenAmount::ZERO,
        src_chain: ChainId::BASE,
        src_payer: payer(),
        rail: PoolRail::SameChain,
        expires_at: Timestamp::from_nanos(1_000),
        salt: [salt; 32],
        icp_fees: None,
    };
    let r#ref = register(&mut pool, terms);
    pool.step(PoolStep::Arrived {
        r#ref,
        credit: credit(log),
        amount: base,
        block: BlockNumber::new(10),
    })
    .unwrap();
    let shares = pool.ledger.mint_now(base).unwrap();
    pool.step(PoolStep::Minted {
        r#ref,
        owner: PoolAccount::Evm(staker()),
        shares,
    })
    .unwrap();
    pool
}

/// `pool` with `amount` of ckUSDC staked on ICP for `staker()`: shares worth more than the
/// Base pocket holds.
fn with_icp_for_staker(mut pool: Pool, amount: TokenAmount) -> Pool {
    let id = crate::hash::PoolOpId::new([0xef; 32]);
    let owner = PoolAccount::Evm(staker());
    pool.step(PoolStep::StakeStarted {
        id,
        owner,
        from: recipient(201),
        amount,
        created_at: Timestamp::from_nanos(1),
    })
    .unwrap();
    let shares = pool.ledger.mint_now(amount).unwrap();
    pool.step(PoolStep::StakedIcp {
        id,
        owner,
        amount,
        rebalance_share: TokenAmount::ZERO,
        shares,
        block: 2,
    })
    .unwrap();
    pool
}

fn withdrawal(pool: &Pool, usdc_out: TokenAmount, nonce: u64) -> (PayoutKind, Undo) {
    let burned = Shares::burn_for(
        usdc_out,
        pool.ledger.total_assets(),
        pool.ledger.total_shares,
    )
    .unwrap();
    let kind = PayoutKind::Withdrawal(EvmWithdrawal {
        staker: staker(),
        usdc: usdc_out,
        max_shares: burned,
        rebalance_share: TokenAmount::from(1_000_u32),
        network_fee: TokenAmount::from(50_000_u32),
        nonce,
        burned,
    });
    let undo = Undo::Remint {
        owner: PoolAccount::Evm(staker()),
        shares: burned,
    };
    (kind, undo)
}

fn icp_swap(min_usdc: TokenAmount) -> (PayoutKind, Undo) {
    let swap = IcpSwapTerms {
        token_in: candid::Principal::from_slice(&[0x31; 10]),
        amount_in: min_usdc,
        from: recipient(5),
        min_usdc,
        premium: TokenAmount::from(10_000_u32),
        network_fee: TokenAmount::from(50_000_u32),
        refund_to: recipient(6),
        slippage_bps: None,
        planned: None,
    };
    (
        PayoutKind::IcpSwap(swap),
        Undo::Refund { to: swap.refund_to },
    )
}

fn open(pool: &mut Pool, kind: PayoutKind, undo: Undo) -> u64 {
    let id = pool.ledger.lines().next_id;
    pool.step(PoolStep::PayoutOpened {
        id,
        kind: Box::new(kind),
        terms: terms(),
        undo,
        plan: None,
    })
    .unwrap();
    id
}

fn order(id: u8) -> PinnedOrder {
    PinnedOrder {
        order_id: [id; 32],
        order: vec![id; 4],
        gas_limit: 400_000,
        deadline_s: 1_800_000_000,
    }
}

fn signed(id: u64, order: PinnedOrder) -> PoolStep {
    PoolStep::PayoutSigned {
        id,
        order,
        chain_id: ChainId::BASE,
        nonce: Nonce::new(3),
        tx_hash: TxHash::new([0x77; 32]),
        raw_tx: vec![0x02, 0x03],
    }
}

fn status(pool: &Pool, id: u64) -> PayoutStatus {
    pool.payouts[&id].status.clone()
}

#[test]
fn payout_settings_are_refused_when_they_do_not_hold_together() {
    let mut pool = Pool::default();
    let good = PayoutSettings::default();
    for bad in [
        PayoutSettings {
            rebalance_share_floor_bps: 10_001,
            ..good
        },
        PayoutSettings {
            order_wait: std::time::Duration::ZERO,
            ..good
        },
        PayoutSettings {
            order_wait: std::time::Duration::from_secs(3_601),
            ..good
        },
        PayoutSettings {
            max_payout_gas: 20_999,
            ..good
        },
        PayoutSettings {
            max_payout_gas: 10_000_001,
            ..good
        },
    ] {
        assert!(matches!(
            pool.step(PoolStep::PayoutSettingsSet { settings: bad }),
            Err(PoolStepError::PayoutSettings(_))
        ));
    }
    pool.step(PoolStep::PayoutSettingsSet { settings: good })
        .unwrap();
    assert_eq!(pool.ledger.lines().settings, Some(good));
}

#[test]
fn a_withdrawal_moves_the_base_pocket_at_each_step() {
    let mut pool = with_base(funded(usdc(1_000)), usdc(5_000), 0x41, 1);
    let shares_before = pool.ledger.total_shares;
    let assets_before = pool.ledger.total_assets();
    let (kind, undo) = withdrawal(&pool, usdc(1_000), 7);
    let PayoutKind::Withdrawal(w) = kind else {
        unreachable!()
    };
    let id = open(&mut pool, kind, undo);
    // the burn and the nonce at the opening
    assert_eq!(
        pool.ledger.total_shares,
        shares_before.checked_sub(w.burned).unwrap()
    );
    assert!(pool.nonces.contains(&crate::pool::NonceUse {
        staker: staker(),
        nonce: 7
    }));
    let amount = w.payout_amount().unwrap();
    assert_eq!(amount, TokenAmount::from(1_000_000_000_u64 - 51_000));
    pool.step(PoolStep::PayoutAmountKnown {
        id,
        amount,
        received: None,
    })
    .unwrap();
    // the stakers lose exactly the withdrawal less its rebalancing share
    assert_eq!(
        pool.ledger.total_assets(),
        assets_before
            .checked_sub(usdc(1_000))
            .unwrap()
            .checked_add(w.rebalance_share)
            .unwrap()
    );
    let base = pool.ledger.pockets.base;
    pool.step(PoolStep::PayoutWaiting { id }).unwrap();
    let hold = amount.checked_add(w.network_fee).unwrap();
    assert_eq!(
        pool.ledger.pockets.base.free,
        base.free.checked_sub(hold).unwrap()
    );
    assert_eq!(pool.ledger.pockets.base.reserved, hold);
    pool.step(signed(id, order(1))).unwrap();
    assert_eq!(status(&pool, id), PayoutStatus::Signed(0));
    assert_eq!(pool.ledger.pockets.base.reserved, w.network_fee);
    assert_eq!(pool.ledger.pockets.base.in_flight, amount);
    // signed once: a second signature is refused
    assert!(matches!(
        pool.step(signed(id, order(1))),
        Err(PoolStepError::PayoutNotFrom { .. })
    ));
    pool.step(PoolStep::PayoutPaid {
        id,
        tx_hash: TxHash::new([0x77; 32]),
        block: BlockNumber::new(20),
        intent: None,
    })
    .unwrap();
    assert_eq!(status(&pool, id), PayoutStatus::Paid);
    let base = pool.ledger.pockets.base;
    assert_eq!(base.reserved, TokenAmount::ZERO);
    assert_eq!(base.in_flight, TokenAmount::ZERO);
    assert_eq!(pool.ledger.lines().fees_base, w.network_fee);
    assert_eq!(pool.ledger.owed, TokenAmount::ZERO);
    assert!(pool.ledger.lines().open.is_empty());
    // the fee stays at the Base address, outside the stakers' assets
    assert_eq!(
        pool.ledger.base_held(),
        base.free.checked_add(w.network_fee).unwrap()
    );
    // never undone once paid
    assert!(pool
        .step(PoolStep::PayoutUndone {
            id,
            why: "late".to_string(),
            block: None,
        })
        .is_err());
}

#[test]
fn a_withdrawal_is_refused_on_a_used_nonce_a_small_burn_or_a_burn_above_its_cap() {
    let mut pool = with_base(Pool::default(), usdc(5_000), 0x42, 1);
    let (kind, undo) = withdrawal(&pool, usdc(1_000), 7);
    open(&mut pool, kind, undo);
    let (kind, undo) = withdrawal(&pool, usdc(1_000), 7);
    let id = pool.ledger.lines().next_id;
    let step = |kind, undo| PoolStep::PayoutOpened {
        id,
        kind: Box::new(kind),
        terms: terms(),
        undo,
        plan: None,
    };
    assert_eq!(
        pool.step(step(kind, undo)),
        Err(PoolStepError::NonceUsed(7))
    );
    let (kind, undo) = withdrawal(&pool, usdc(1_000), 8);
    let PayoutKind::Withdrawal(w) = kind else {
        unreachable!()
    };
    let small = Shares::new(w.burned.get() - 1);
    let short = PayoutKind::Withdrawal(EvmWithdrawal { burned: small, ..w });
    let short_undo = Undo::Remint {
        owner: PoolAccount::Evm(staker()),
        shares: small,
    };
    assert!(matches!(
        pool.step(step(short, short_undo)),
        Err(PoolStepError::BurnTooSmall { .. })
    ));
    let capped = PayoutKind::Withdrawal(EvmWithdrawal {
        max_shares: small,
        ..w
    });
    assert!(matches!(
        pool.step(step(capped, undo)),
        Err(PoolStepError::Terms(_))
    ));
    // an undo that is not the burn's own is refused
    let wrong = Undo::Remint {
        owner: PoolAccount::Evm(staker()),
        shares: Shares::new(1),
    };
    assert_eq!(pool.step(step(kind, wrong)), Err(PoolStepError::Mismatch));
    // the id is the next one, and only it
    assert_eq!(
        pool.step(PoolStep::PayoutOpened {
            id: id + 1,
            kind: Box::new(kind),
            terms: terms(),
            undo,
            plan: None,
        }),
        Err(PoolStepError::PayoutId { expected: id })
    );
}

#[test]
fn payouts_wait_first_in_first_out_and_the_queue_outranks_a_new_one() {
    let pool = with_base(Pool::default(), usdc(2_000), 0x43, 1);
    let mut pool = with_icp_for_staker(pool, usdc(3_000));
    let known = |pool: &mut Pool, usdc_out: u64, nonce: u64| {
        let (kind, undo) = withdrawal(pool, usdc(usdc_out), nonce);
        let PayoutKind::Withdrawal(w) = kind else {
            unreachable!()
        };
        let id = open(pool, kind, undo);
        pool.step(PoolStep::PayoutAmountKnown {
            id,
            amount: w.payout_amount().unwrap(),
            received: None,
        })
        .unwrap();
        id
    };
    let first = known(&mut pool, 1_500, 1);
    pool.step(PoolStep::PayoutWaiting { id: first }).unwrap();
    // the pocket holds about 500 more: a 400 payout could wait, but not ahead of a queued one
    let second = known(&mut pool, 600, 2);
    assert!(matches!(
        pool.step(PoolStep::PayoutWaiting { id: second }),
        Err(PoolStepError::BaseShort { .. })
    ));
    pool.step(PoolStep::PayoutQueued { id: second }).unwrap();
    let third = known(&mut pool, 100, 3);
    assert!(matches!(
        pool.step(PoolStep::PayoutWaiting { id: third }),
        Err(PoolStepError::BaseShort { .. })
    ));
    pool.step(PoolStep::PayoutQueued { id: third }).unwrap();
    assert_eq!(pool.ledger.lines().queue_len, 2);
    // the third may not leave ahead of the second, though the pocket covers it
    assert!(matches!(
        pool.step(PoolStep::PayoutWaiting { id: third }),
        Err(PoolStepError::Terms(_))
    ));
    // the head is not covered yet
    assert!(matches!(
        pool.step(PoolStep::PayoutWaiting { id: second }),
        Err(PoolStepError::BaseShort { .. })
    ));
    // the pocket refills: the head leaves first, then the next
    pool = with_base(pool, usdc(1_000), 0x44, 2);
    pool.step(PoolStep::PayoutWaiting { id: second }).unwrap();
    pool.step(PoolStep::PayoutWaiting { id: third }).unwrap();
    let lines = pool.ledger.lines();
    assert_eq!(lines.queue_len, 0);
    assert_eq!(lines.queued, TokenAmount::ZERO);
    assert!(pool.queue.is_empty());
    assert_eq!(status(&pool, third), PayoutStatus::Waiting);
}

#[test]
fn an_icp_swap_holds_its_ckusdc_until_paid_and_its_premium_is_the_pools() {
    let mut pool = with_base(funded(usdc(1_000)), usdc(5_000), 0x45, 1);
    let assets = pool.ledger.total_assets();
    let (kind, undo) = icp_swap(usdc(100));
    let id = open(&mut pool, kind, undo);
    let amount = TokenAmount::from(100_000_000_u64 - 60_000);
    // the amount must be what the ckUSDC less the premium and the fee is
    assert!(matches!(
        pool.step(PoolStep::PayoutAmountKnown {
            id,
            amount: TokenAmount::from(100_000_000_u64 - 50_000),
            received: Some(usdc(100)),
        }),
        Err(PoolStepError::Terms(_))
    ));
    pool.step(PoolStep::PayoutAmountKnown {
        id,
        amount,
        received: Some(usdc(100)),
    })
    .unwrap();
    let icp = pool.ledger.pockets.icp;
    assert_eq!(icp.reserved, TokenAmount::from(100_000_000_u64 - 50_000));
    assert_eq!(pool.ledger.lines().fees_icp, TokenAmount::from(50_000_u32));
    assert_eq!(pool.ledger.owed, amount);
    // the premium is the stakers' from now
    assert_eq!(
        pool.ledger.total_assets(),
        assets.checked_add(TokenAmount::from(10_000_u32)).unwrap()
    );
    // the ICP check expects the fee too
    assert_eq!(
        pool.ledger.icp_held(),
        icp.free
            .checked_add(icp.reserved)
            .unwrap()
            .checked_add(TokenAmount::from(50_000_u32))
            .unwrap()
    );
    pool.step(PoolStep::PayoutWaiting { id }).unwrap();
    pool.step(signed(id, order(3))).unwrap();
    pool.step(PoolStep::PayoutPaid {
        id,
        tx_hash: TxHash::new([0x77; 32]),
        block: BlockNumber::new(30),
        intent: Some([0x99; 32]),
    })
    .unwrap();
    pool.step(PoolStep::PayoutDelivered {
        id,
        block: BlockNumber::new(31),
    })
    .unwrap();
    // the ckUSDC is the pool's free money now; the USDC left Base
    assert_eq!(pool.ledger.pockets.icp.reserved, TokenAmount::ZERO);
    assert_eq!(
        pool.ledger.pockets.icp.free,
        icp.free
            .checked_add(TokenAmount::from(100_000_000_u64 - 50_000))
            .unwrap()
    );
    assert_eq!(pool.ledger.owed, TokenAmount::ZERO);
    assert_eq!(
        pool.ledger.total_assets(),
        assets.checked_add(TokenAmount::from(10_000_u32)).unwrap()
    );
    assert_eq!(pool.payouts[&id].intent, Some([0x99; 32]));
}

#[test]
fn an_undo_is_exact() {
    let mut pool = with_base(funded(usdc(1_000)), usdc(5_000), 0x46, 1);
    let before = pool.ledger.clone();
    let staker_shares = pool.shares_of_evm(staker());
    // an ICP swap undone while it waits: every line back
    let (kind, undo) = icp_swap(usdc(100));
    let id = open(&mut pool, kind, undo);
    pool.step(PoolStep::PayoutAmountKnown {
        id,
        amount: TokenAmount::from(100_000_000_u64 - 60_000),
        received: Some(usdc(100)),
    })
    .unwrap();
    pool.step(PoolStep::PayoutWaiting { id }).unwrap();
    pool.step(PoolStep::PayoutUndoing {
        id,
        why: "past its deadline".to_string(),
    })
    .unwrap();
    // nothing is signed for it once its undo began
    assert!(pool.step(signed(id, order(4))).is_err());
    pool.step(PoolStep::PayoutUndone {
        id,
        why: "past its deadline".to_string(),
        block: Some(77),
    })
    .unwrap();
    assert_eq!(pool.ledger.pockets, before.pockets);
    assert_eq!(pool.ledger.owed, before.owed);
    assert_eq!(pool.ledger.total_assets(), before.total_assets());
    assert_eq!(pool.ledger.lines().fees_icp, TokenAmount::ZERO);
    assert_eq!(pool.payouts[&id].icp_block, Some(77));
    // a withdrawal undone from the queue: exactly its shares back, the price as it was
    let (kind, undo) = withdrawal(&pool, usdc(4_900), 9);
    let PayoutKind::Withdrawal(w) = kind else {
        unreachable!()
    };
    let id = open(&mut pool, kind, undo);
    pool.step(PoolStep::PayoutAmountKnown {
        id,
        amount: w.payout_amount().unwrap(),
        received: None,
    })
    .unwrap();
    let other = pool.ledger.lines();
    assert_eq!(other.queue_len, 0);
    // 4,900 less its fees is covered by the 5,000: it would wait; queue it by hand
    pool.step(PoolStep::PayoutQueued { id }).unwrap();
    pool.step(PoolStep::PayoutUndone {
        id,
        why: "past its deadline".to_string(),
        block: None,
    })
    .unwrap();
    assert_eq!(pool.ledger.pockets, before.pockets);
    assert_eq!(pool.ledger.owed, before.owed);
    assert_eq!(pool.ledger.total_shares, before.total_shares);
    assert_eq!(pool.ledger.total_assets(), before.total_assets());
    assert_eq!(pool.shares_of_evm(staker()), staker_shares);
    assert_eq!(pool.ledger.lines().queue_len, 0);
    assert!(pool.queue.is_empty());
}

#[test]
fn a_payout_known_and_unpaid_prices_a_stake_on_assets_less_owed() {
    let mut pool = with_base(funded(usdc(1_000)), usdc(5_000), 0x47, 1);
    let (kind, undo) = withdrawal(&pool, usdc(1_000), 1);
    let PayoutKind::Withdrawal(w) = kind else {
        unreachable!()
    };
    let id = open(&mut pool, kind, undo);
    pool.step(PoolStep::PayoutAmountKnown {
        id,
        amount: w.payout_amount().unwrap(),
        received: None,
    })
    .unwrap();
    pool.step(PoolStep::PayoutWaiting { id }).unwrap();
    let assets = pool
        .ledger
        .pockets
        .assets()
        .checked_sub(pool.ledger.owed)
        .unwrap();
    assert_eq!(pool.ledger.total_assets(), assets);
    // a stake in between mints at exactly that price
    let shares = pool.ledger.total_shares;
    let expected = Shares::mint_for(usdc(500), assets, shares).unwrap();
    pool = with_base(pool, usdc(500), 0x48, 2);
    assert_eq!(
        pool.ledger.total_shares,
        shares.checked_add(expected).unwrap()
    );
    // and a withdrawal in between burns at it
    let least = Shares::burn_for(
        usdc(10),
        pool.ledger.total_assets(),
        pool.ledger.total_shares,
    )
    .unwrap();
    let (kind, _) = withdrawal(&pool, usdc(10), 2);
    let PayoutKind::Withdrawal(w) = kind else {
        unreachable!()
    };
    assert_eq!(w.burned, least);
}

#[test]
fn a_step_out_of_order_is_refused() {
    let mut pool = with_base(Pool::default(), usdc(5_000), 0x49, 1);
    let (kind, undo) = icp_swap(usdc(100));
    let id = open(&mut pool, kind, undo);
    // nothing waits or is queued before its amount is known
    assert!(pool.step(PoolStep::PayoutWaiting { id }).is_err());
    assert!(pool.step(PoolStep::PayoutQueued { id }).is_err());
    assert!(pool.step(signed(id, order(5))).is_err());
    assert!(pool
        .step(PoolStep::PayoutPaid {
            id,
            tx_hash: TxHash::new([0; 32]),
            block: BlockNumber::new(1),
            intent: None,
        })
        .is_err());
    // a withdrawal's amount is never an ICP swap's
    assert_eq!(
        pool.step(PoolStep::PayoutAmountKnown {
            id,
            amount: usdc(1),
            received: None,
        }),
        Err(PoolStepError::Mismatch)
    );
    // an unknown payout
    assert_eq!(
        pool.step(PoolStep::PayoutWaiting { id: 99 }),
        Err(PoolStepError::UnknownPayout(99))
    );
}

impl Pool {
    fn shares_of_evm(&self, address: EvmAddress) -> Shares {
        use crate::pool::PoolLookup;
        self.shares_of(&PoolAccount::Evm(address).key())
    }
}

/// A stake from EVM carries at least the canister's rebalancing share floor once the payout
/// settings are set (interface section 3: "P11b adds the canister's floor").
#[test]
fn a_stakes_rebalancing_share_meets_the_floor() {
    use crate::pool::check_arrival;
    let mut pool = with_base(Pool::default(), usdc(10), 0x4a, 1);
    let terms = |rebalance_share: u64| Registration {
        kind: DepositKind::Stake {
            owner: PoolAccount::Evm(staker()),
        },
        min_usdc: usdc(100),
        premium: TokenAmount::ZERO,
        rebalance_share: TokenAmount::from(rebalance_share),
        src_chain: ChainId::ARBITRUM,
        src_payer: payer(),
        rail: PoolRail::Cctp,
        expires_at: Timestamp::from_nanos(1_000),
        salt: [0x4b; 32],
        icp_fees: None,
    };
    // 2 bps of 100 USDC is 20,000
    assert!(matches!(
        check_arrival(&pool.ledger, &terms(19_999)),
        Err(crate::pool::RegisterRefusal::NotAllowed(_))
    ));
    assert_eq!(check_arrival(&pool.ledger, &terms(20_000)), Ok(()));
    pool.ledger.payouts = None;
    assert_eq!(check_arrival(&pool.ledger, &terms(0)), Ok(()));
}

/// The review's double release (2026-10-23, `PayoutPaid`): an ICP swap whose ckUSDC went
/// free to the pool once never frees it again, so a payout paid a second time (as one reopened
/// by an Eco refund was) adds nothing to the free ckUSDC.
#[test]
fn a_paid_payout_releases_its_ckusdc_once() {
    let mut pool = with_base(funded(usdc(1_000)), usdc(5_000), 0x4d, 1);
    let (kind, undo) = icp_swap(usdc(100));
    let id = open(&mut pool, kind, undo);
    pool.step(PoolStep::PayoutAmountKnown {
        id,
        amount: TokenAmount::from(100_000_000_u64 - 60_000),
        received: Some(usdc(100)),
    })
    .unwrap();
    pool.step(PoolStep::PayoutWaiting { id }).unwrap();
    pool.step(signed(id, order(7))).unwrap();
    // as a payout paid once stands: its ckUSDC free already
    let hold = pool.payouts[&id].icp_hold();
    let icp = &mut pool.ledger.pockets.icp;
    icp.reserved = icp.reserved.checked_sub(hold).unwrap();
    icp.free = icp.free.checked_add(hold).unwrap();
    pool.payouts.get_mut(&id).unwrap().icp_released = crate::pool::payout::Flag(true);
    let icp = pool.ledger.pockets.icp;
    pool.step(PoolStep::PayoutPaid {
        id,
        tx_hash: TxHash::new([0x77; 32]),
        block: BlockNumber::new(31),
        intent: None,
    })
    .unwrap();
    assert_eq!(pool.ledger.pockets.icp, icp);
    assert!(pool.payouts[&id].icp_released.0);
    // nor once its Eco intent is read filled
    let mut payout = pool.payouts[&id].clone();
    payout.status = PayoutStatus::Delivering;
    pool.payouts.insert(id, payout);
    pool.ledger.pockets.base.in_flight = pool.payouts[&id].amount.unwrap();
    pool.step(PoolStep::PayoutDelivered {
        id,
        block: BlockNumber::new(32),
    })
    .unwrap();
    assert_eq!(pool.ledger.pockets.icp, icp);
}

fn paid(id: u64, intent: Option<[u8; 32]>) -> PoolStep {
    PoolStep::PayoutPaid {
        id,
        tx_hash: TxHash::new([0x77; 32]),
        block: BlockNumber::new(30),
        intent,
    }
}

/// Item 6 of the final review fixes: a payout is signed once. A revert never brings it back
/// to waiting for a signature, its pinned order included.
#[test]
fn a_payout_is_signed_once() {
    let mut pool = with_base(funded(usdc(1_000)), usdc(5_000), 0x4e, 1);
    let (kind, undo) = icp_swap(usdc(100));
    let id = open(&mut pool, kind, undo);
    pool.step(PoolStep::PayoutAmountKnown {
        id,
        amount: TokenAmount::from(100_000_000_u64 - 60_000),
        received: Some(usdc(100)),
    })
    .unwrap();
    pool.step(PoolStep::PayoutWaiting { id }).unwrap();
    pool.step(signed(id, order(8))).unwrap();
    pool.step(PoolStep::PayoutReverted { id, block: None })
        .unwrap();
    assert_eq!(status(&pool, id), PayoutStatus::Reverted);
    for order in [order(8), order(9)] {
        assert!(matches!(
            pool.step(signed(id, order)),
            Err(PoolStepError::PayoutNotFrom { .. })
        ));
    }
    // nor does it wait for an order again
    assert!(pool.step(PoolStep::PayoutWaiting { id }).is_err());
    assert!(pool.step(PoolStep::PayoutRelisted { id }).is_err());
}

/// A reverted ICP swap is undone once: its refund begins, its USDC goes back to the Base
/// pocket's free line, its ckUSDC leaves with the refund, and every line ends where it was.
#[test]
fn a_reverted_icp_swap_is_undone_once_and_every_line_returns() {
    let mut pool = with_base(funded(usdc(1_000)), usdc(5_000), 0x4f, 1);
    let before = pool.ledger.clone();
    let (kind, undo) = icp_swap(usdc(100));
    let id = open(&mut pool, kind, undo);
    pool.step(PoolStep::PayoutAmountKnown {
        id,
        amount: TokenAmount::from(100_000_000_u64 - 60_000),
        received: Some(usdc(100)),
    })
    .unwrap();
    pool.step(PoolStep::PayoutWaiting { id }).unwrap();
    pool.step(signed(id, order(10))).unwrap();
    pool.step(PoolStep::PayoutReverted { id, block: None })
        .unwrap();
    let why = || "its transaction reverted".to_string();
    pool.step(PoolStep::PayoutUndoing { id, why: why() })
        .unwrap();
    pool.step(PoolStep::PayoutUndone {
        id,
        why: why(),
        block: Some(12),
    })
    .unwrap();
    assert_eq!(status(&pool, id), PayoutStatus::Undone(why()));
    assert_eq!(pool.ledger.pockets, before.pockets);
    assert_eq!(pool.ledger.owed, before.owed);
    assert_eq!(pool.ledger.total_assets(), before.total_assets());
    assert_eq!(pool.ledger.lines().fees_icp, TokenAmount::ZERO);
    assert!(pool.ledger.lines().open.is_empty());
    // undone once: nothing more
    assert!(pool
        .step(PoolStep::PayoutUndoing { id, why: why() })
        .is_err());
    assert!(pool
        .step(PoolStep::PayoutUndone {
            id,
            why: why(),
            block: Some(13),
        })
        .is_err());
    assert!(pool.step(paid(id, None)).is_err());
}

/// A reverted withdrawal mints back exactly the shares it burned, once.
#[test]
fn a_reverted_withdrawal_is_reminted_once() {
    let mut pool = with_base(funded(usdc(1_000)), usdc(5_000), 0x50, 1);
    let before = pool.ledger.clone();
    let shares = pool.shares_of_evm(staker());
    let (kind, undo) = withdrawal(&pool, usdc(1_000), 11);
    let PayoutKind::Withdrawal(w) = kind else {
        unreachable!()
    };
    let id = open(&mut pool, kind, undo);
    pool.step(PoolStep::PayoutAmountKnown {
        id,
        amount: w.payout_amount().unwrap(),
        received: None,
    })
    .unwrap();
    pool.step(PoolStep::PayoutWaiting { id }).unwrap();
    pool.step(signed(id, order(11))).unwrap();
    pool.step(PoolStep::PayoutReverted { id, block: None })
        .unwrap();
    let undone = || PoolStep::PayoutUndone {
        id,
        why: "its transaction reverted".to_string(),
        block: None,
    };
    pool.step(undone()).unwrap();
    assert_eq!(pool.ledger.pockets, before.pockets);
    assert_eq!(pool.ledger.owed, before.owed);
    assert_eq!(pool.ledger.total_shares, before.total_shares);
    assert_eq!(pool.shares_of_evm(staker()), shares);
    assert!(pool.step(undone()).is_err());
}

/// `pool` with an ICP swap of 100 ckUSDC (or a withdrawal of 1,000 USDC) paid on Base into an
/// Eco intent: its id.
fn delivering(pool: &mut Pool, withdrawing: bool) -> u64 {
    let (kind, undo, amount, received) = if withdrawing {
        let (kind, undo) = withdrawal(pool, usdc(1_000), 21);
        let PayoutKind::Withdrawal(w) = kind else {
            unreachable!()
        };
        (kind, undo, w.payout_amount().unwrap(), None)
    } else {
        let (kind, undo) = icp_swap(usdc(100));
        (
            kind,
            undo,
            TokenAmount::from(100_000_000_u64 - 60_000),
            Some(usdc(100)),
        )
    };
    let id = open(pool, kind, undo);
    pool.step(PoolStep::PayoutAmountKnown {
        id,
        amount,
        received,
    })
    .unwrap();
    pool.step(PoolStep::PayoutWaiting { id }).unwrap();
    pool.step(signed(id, order(20))).unwrap();
    pool.step(paid(id, Some([0xa0; 32]))).unwrap();
    id
}

fn refunded(id: u64, amount: TokenAmount, log: u64) -> PoolStep {
    PoolStep::PayoutRefunded {
        id,
        credit: crate::pool::CreditKey {
            chain: ChainId::BASE,
            tx: TxHash::new([0xa1; 32]),
            log_index: log,
        },
        amount,
        block: BlockNumber::new(40),
    }
}

fn unfilled(id: u64) -> PoolStep {
    PoolStep::PayoutUnfilled {
        id,
        block: BlockNumber::new(38),
    }
}

/// Every step that would pay `id` again is refused.
fn never_paid_again(pool: &mut Pool, id: u64) {
    for step in [
        PoolStep::PayoutWaiting { id },
        PoolStep::PayoutRelisted { id },
        signed(id, order(21)),
        paid(id, None),
        PoolStep::PayoutDelivered {
            id,
            block: BlockNumber::new(39),
        },
    ] {
        assert!(pool.step(step).is_err(), "{id} paid again");
    }
}

/// An Eco payout waits, all its lines as they were, until its intent's fill is read; read
/// filled, it is paid then, exactly as a payout with no intent is at once.
#[test]
fn an_eco_payout_is_delivering_until_its_fill_is_read() {
    let mut pool = with_base(funded(usdc(1_000)), usdc(5_000), 0x51, 1);
    let (kind, undo) = icp_swap(usdc(100));
    let id = open(&mut pool, kind, undo);
    let amount = TokenAmount::from(100_000_000_u64 - 60_000);
    pool.step(PoolStep::PayoutAmountKnown {
        id,
        amount,
        received: Some(usdc(100)),
    })
    .unwrap();
    pool.step(PoolStep::PayoutWaiting { id }).unwrap();
    pool.step(signed(id, order(22))).unwrap();
    let signed_ledger = pool.ledger.clone();
    pool.step(paid(id, Some([0xa2; 32]))).unwrap();
    assert_eq!(status(&pool, id), PayoutStatus::Delivering);
    assert_eq!(pool.ledger.pockets, signed_ledger.pockets);
    assert_eq!(pool.ledger.owed, signed_ledger.owed);
    assert!(pool.ledger.lines().open.contains(&id));
    assert_eq!(pool.payouts[&id].intent, Some([0xa2; 32]));
    // a second close is refused
    assert!(pool.step(paid(id, Some([0xa2; 32]))).is_err());
    pool.step(PoolStep::PayoutDelivered {
        id,
        block: BlockNumber::new(33),
    })
    .unwrap();
    assert_eq!(status(&pool, id), PayoutStatus::Paid);
    let icp = pool.ledger.pockets.icp;
    assert_eq!(icp.reserved, TokenAmount::ZERO);
    assert_eq!(
        icp.free,
        signed_ledger
            .pockets
            .icp
            .free
            .checked_add(
                usdc(100)
                    .checked_sub(TokenAmount::from(50_000_u32))
                    .unwrap()
            )
            .unwrap()
    );
    assert_eq!(pool.ledger.pockets.base.in_flight, TokenAmount::ZERO);
    assert_eq!(pool.ledger.owed, TokenAmount::ZERO);
    assert!(pool.ledger.lines().open.is_empty());
    // paid: never unfilled, refunded or delivered again
    assert!(pool.step(unfilled(id)).is_err());
    assert!(pool.step(refunded(id, amount, 1)).is_err());
    never_paid_again(&mut pool, id);
}

/// Item 7 of the final review fixes, read first: the intent read unfilled past its route
/// deadline undoes the payout at once, the user refunded; Eco's refund then credits the Base
/// pocket once, and nothing ever pays the payout again.
#[test]
fn an_unfilled_intent_refunds_the_user_then_eco_credits_the_pool_once() {
    let mut pool = with_base(funded(usdc(1_000)), usdc(5_000), 0x52, 1);
    let before = pool.ledger.clone();
    let id = delivering(&mut pool, false);
    let amount = pool.payouts[&id].amount.unwrap();
    pool.step(unfilled(id)).unwrap();
    assert_eq!(status(&pool, id), PayoutStatus::Unfilled);
    assert_eq!(
        pool.payouts[&id].reclaim,
        Some(crate::pool::payout::Reclaim::Pending)
    );
    // the USDC is owed back by Eco: in flight still
    assert_eq!(pool.ledger.pockets.base.in_flight, amount);
    assert!(pool.step(unfilled(id)).is_err());
    never_paid_again(&mut pool, id);
    let why = || "its Eco intent was not filled".to_string();
    pool.step(PoolStep::PayoutUndoing { id, why: why() })
        .unwrap();
    pool.step(PoolStep::PayoutUndone {
        id,
        why: why(),
        block: Some(14),
    })
    .unwrap();
    never_paid_again(&mut pool, id);
    // Eco's refund: credited once, to the Base pocket only
    assert!(pool.step(refunded(id, usdc(1), 1)).is_err());
    pool.step(refunded(id, amount, 1)).unwrap();
    assert_eq!(status(&pool, id), PayoutStatus::Undone(why()));
    assert_eq!(
        pool.payouts[&id].reclaim,
        Some(crate::pool::payout::Reclaim::Credited)
    );
    assert!(pool.step(refunded(id, amount, 2)).is_err());
    never_paid_again(&mut pool, id);
    // every line where it was before the swap
    assert_eq!(pool.ledger.pockets, before.pockets);
    assert_eq!(pool.ledger.owed, before.owed);
    assert_eq!(pool.ledger.total_assets(), before.total_assets());
    assert_eq!(pool.ledger.lines().fees_icp, TokenAmount::ZERO);
    assert!(pool.ledger.lines().open.is_empty());
}

/// Item 7, Eco's refund reported first: the same one user refund and one pool credit.
#[test]
fn an_eco_refund_first_refunds_the_user_and_credits_the_pool_once() {
    let mut pool = with_base(funded(usdc(1_000)), usdc(5_000), 0x53, 1);
    let before = pool.ledger.clone();
    let id = delivering(&mut pool, false);
    let amount = pool.payouts[&id].amount.unwrap();
    pool.step(refunded(id, amount, 1)).unwrap();
    assert_eq!(status(&pool, id), PayoutStatus::Unfilled);
    assert_eq!(
        pool.payouts[&id].reclaim,
        Some(crate::pool::payout::Reclaim::Credited)
    );
    assert_eq!(pool.ledger.pockets.base.in_flight, TokenAmount::ZERO);
    // its fill read later finds nothing to do; a second refund nothing to credit
    assert!(pool.step(unfilled(id)).is_err());
    assert!(pool.step(refunded(id, amount, 2)).is_err());
    never_paid_again(&mut pool, id);
    let why = || "its Eco intent was not filled".to_string();
    pool.step(PoolStep::PayoutUndoing { id, why: why() })
        .unwrap();
    pool.step(PoolStep::PayoutUndone {
        id,
        why: why(),
        block: Some(15),
    })
    .unwrap();
    never_paid_again(&mut pool, id);
    assert!(pool.step(refunded(id, amount, 3)).is_err());
    assert_eq!(pool.ledger.pockets, before.pockets);
    assert_eq!(pool.ledger.owed, before.owed);
    assert_eq!(pool.ledger.total_assets(), before.total_assets());
    assert_eq!(pool.ledger.lines().fees_icp, TokenAmount::ZERO);
}

/// Both orders of an unfilled intent end in the same ledger, an ICP swap's and a
/// withdrawal's.
#[test]
fn both_orders_of_an_unfilled_intent_end_in_the_same_ledger() {
    for withdrawing in [false, true] {
        let end = |read_first: bool| {
            let mut pool = with_base(funded(usdc(1_000)), usdc(5_000), 0x54, 1);
            let id = delivering(&mut pool, withdrawing);
            let amount = pool.payouts[&id].amount.unwrap();
            let undo = |pool: &mut Pool| {
                if !withdrawing {
                    pool.step(PoolStep::PayoutUndoing {
                        id,
                        why: "unfilled".to_string(),
                    })
                    .unwrap();
                }
                pool.step(PoolStep::PayoutUndone {
                    id,
                    why: "unfilled".to_string(),
                    block: None,
                })
                .unwrap();
            };
            if read_first {
                pool.step(unfilled(id)).unwrap();
                undo(&mut pool);
                pool.step(refunded(id, amount, 1)).unwrap();
            } else {
                pool.step(refunded(id, amount, 1)).unwrap();
                undo(&mut pool);
            }
            pool
        };
        let (first, second) = (end(true), end(false));
        assert_eq!(first.ledger, second.ledger, "withdrawing: {withdrawing}");
        assert_eq!(first.shares, second.shares);
        assert_eq!(first.credited, second.credited);
    }
}

/// An unfilled withdrawal mints back exactly its shares and the pool is credited once, in
/// both orders; its network fee goes back to the Base pocket's free line.
#[test]
fn an_unfilled_withdrawal_is_reminted_and_the_pool_credited_once() {
    for read_first in [true, false] {
        let mut pool = with_base(funded(usdc(1_000)), usdc(5_000), 0x55, 1);
        let before = pool.ledger.clone();
        let shares = pool.shares_of_evm(staker());
        let id = delivering(&mut pool, true);
        let amount = pool.payouts[&id].amount.unwrap();
        if read_first {
            pool.step(unfilled(id)).unwrap();
        } else {
            pool.step(refunded(id, amount, 1)).unwrap();
        }
        never_paid_again(&mut pool, id);
        let undone = || PoolStep::PayoutUndone {
            id,
            why: "unfilled".to_string(),
            block: None,
        };
        pool.step(undone()).unwrap();
        assert!(pool.step(undone()).is_err());
        if read_first {
            pool.step(refunded(id, amount, 1)).unwrap();
        }
        assert!(pool.step(refunded(id, amount, 2)).is_err());
        assert_eq!(
            pool.ledger.pockets, before.pockets,
            "read first: {read_first}"
        );
        assert_eq!(pool.ledger.owed, before.owed);
        assert_eq!(pool.ledger.total_shares, before.total_shares);
        assert_eq!(pool.ledger.lines().fees_base, TokenAmount::ZERO);
        assert_eq!(pool.shares_of_evm(staker()), shares);
    }
}

/// An unfilled payout whose user refund stopped for a human: Eco's refund still credits the
/// Base pocket, once.
#[test]
fn the_eco_refund_of_a_payout_whose_refund_failed_still_credits_the_pool() {
    let mut pool = with_base(funded(usdc(1_000)), usdc(5_000), 0x56, 1);
    let id = delivering(&mut pool, false);
    let amount = pool.payouts[&id].amount.unwrap();
    pool.step(unfilled(id)).unwrap();
    pool.step(PoolStep::PayoutUndoing {
        id,
        why: "unfilled".to_string(),
    })
    .unwrap();
    pool.step(PoolStep::PayoutStopped {
        id,
        why: "ALERT: the refund was refused".to_string(),
    })
    .unwrap();
    let free = pool.ledger.pockets.base.free;
    pool.step(refunded(id, amount, 1)).unwrap();
    assert_eq!(
        pool.ledger.pockets.base.free,
        free.checked_add(amount).unwrap()
    );
    assert_eq!(pool.ledger.pockets.base.in_flight, TokenAmount::ZERO);
    assert!(matches!(status(&pool, id), PayoutStatus::Failed(_)));
    assert!(pool.step(refunded(id, amount, 2)).is_err());
}

/// The lead's check of the deferred Eco accounting: the stakers' assets and the share price
/// stay as they were at the signature through `Delivering`, `PayoutDelivered`,
/// `PayoutUnfilled` and `PayoutRefunded`, in both orders, an ICP swap's and a withdrawal's.
#[test]
fn total_assets_and_the_share_price_hold_through_an_eco_payout() {
    let price = |pool: &Pool| (pool.ledger.total_assets(), pool.ledger.total_shares);
    for withdrawing in [false, true] {
        // filled
        let mut pool = with_base(funded(usdc(1_000)), usdc(5_000), 0x57, 1);
        let id = delivering(&mut pool, withdrawing);
        // `PayoutPaid` of an Eco order moves no line (`an_eco_payout_paid_moves_no_line`):
        // the price delivering is the signature's
        let at_signature = price(&pool);
        pool.step(PoolStep::PayoutDelivered {
            id,
            block: BlockNumber::new(34),
        })
        .unwrap();
        assert_eq!(price(&pool), at_signature, "delivered");
        // unfilled, read first, then refunded
        let mut pool = with_base(funded(usdc(1_000)), usdc(5_000), 0x57, 1);
        let id = delivering(&mut pool, withdrawing);
        let amount = pool.payouts[&id].amount.unwrap();
        pool.step(unfilled(id)).unwrap();
        assert_eq!(price(&pool), at_signature, "unfilled");
        pool.step(refunded(id, amount, 1)).unwrap();
        assert_eq!(price(&pool), at_signature, "refunded after");
        // refunded first
        let mut pool = with_base(funded(usdc(1_000)), usdc(5_000), 0x57, 1);
        let id = delivering(&mut pool, withdrawing);
        pool.step(refunded(id, amount, 1)).unwrap();
        assert_eq!(price(&pool), at_signature, "refunded first");
    }
}

/// `PayoutPaid` of an Eco order moves no line: its ckUSDC, owed line, USDC in flight and a
/// withdrawal's fee stay until its fill is read.
#[test]
fn an_eco_payout_paid_moves_no_line() {
    for withdrawing in [false, true] {
        let mut pool = with_base(funded(usdc(1_000)), usdc(5_000), 0x58, 1);
        let (kind, undo, amount, received) = if withdrawing {
            let (kind, undo) = withdrawal(&pool, usdc(1_000), 21);
            let PayoutKind::Withdrawal(w) = kind else {
                unreachable!()
            };
            (kind, undo, w.payout_amount().unwrap(), None)
        } else {
            let (kind, undo) = icp_swap(usdc(100));
            (
                kind,
                undo,
                TokenAmount::from(100_000_000_u64 - 60_000),
                Some(usdc(100)),
            )
        };
        let id = open(&mut pool, kind, undo);
        pool.step(PoolStep::PayoutAmountKnown {
            id,
            amount,
            received,
        })
        .unwrap();
        pool.step(PoolStep::PayoutWaiting { id }).unwrap();
        pool.step(signed(id, order(20))).unwrap();
        let before = pool.ledger.clone();
        pool.step(paid(id, Some([0xa0; 32]))).unwrap();
        assert_eq!(pool.ledger, before);
    }
}

/// A plan for `planned`: a same-chain order paying USDC out on Base, as `abi.encode`.
fn plan_for(planned: TokenAmount) -> PinnedOrder {
    use crate::swapic::{addr, encode_order, Order, Payout};
    let usdc = EvmAddress::new([0x83; 20]);
    let order = Order {
        r#ref: alloy_primitives::FixedBytes([0x61; 32]),
        payer: addr(EvmAddress::new([0x36; 20])),
        tokenIn: addr(usdc),
        amountIn: alloy_primitives::U256::from_be_bytes(planned.to_be_bytes()),
        deadline: alloy_primitives::U256::from(1_800_000_000_u64),
        steps: vec![],
        payout: Payout {
            token: addr(usdc),
            recipient: addr(payer()),
            minOut: alloy_primitives::U256::from(1_u8),
        },
    };
    PinnedOrder {
        order_id: [0x62; 32],
        order: encode_order(&order),
        gas_limit: 300_000,
        deadline_s: 1_800_000_000,
    }
}

/// A planned swap of `token_in`, its floor `min_usdc`, for `planned`, 100 bps of slippage.
fn planned_swap(
    token_in: candid::Principal,
    min_usdc: TokenAmount,
    planned: TokenAmount,
) -> IcpSwapTerms {
    IcpSwapTerms {
        token_in,
        amount_in: usdc(100),
        from: recipient(5),
        min_usdc,
        premium: TokenAmount::ZERO,
        network_fee: TokenAmount::from(50_000_u32),
        refund_to: recipient(6),
        slippage_bps: Some(100),
        planned: Some(planned),
    }
}

fn open_planned(
    pool: &mut Pool,
    swap: IcpSwapTerms,
    plan: PinnedOrder,
) -> Result<u64, PoolStepError> {
    let id = pool.ledger.lines().next_id;
    pool.step(PoolStep::PayoutOpened {
        id,
        kind: Box::new(PayoutKind::IcpSwap(swap)),
        terms: terms(),
        undo: Undo::Refund { to: swap.refund_to },
        plan: Some(Box::new(plan)),
    })?;
    Ok(id)
}

/// M-01, fixed by the payout plan: a planned swap's premium is the settings' share of what
/// its sale really made, whatever amount the plan named, and its payout is what is left.
#[test]
fn a_planned_amount_takes_the_premium_from_what_the_sale_made() {
    let mut pool = with_base(funded(usdc(1_000)), usdc(5_000), 0x59, 1);
    let token = candid::Principal::from_slice(&[0x31; 10]);
    // a plan for 1 USDC, a sale that made 5,000
    let swap = planned_swap(token, usdc(1), usdc(1));
    let id = open_planned(&mut pool, swap, plan_for(usdc(1))).unwrap();
    let premium = TokenAmount::from(5_000_000_u64);
    let amount = usdc(5_000)
        .checked_sub(premium)
        .and_then(|left| left.checked_sub(TokenAmount::from(50_000_u32)))
        .unwrap();
    pool.step(PoolStep::PayoutAmountKnown {
        id,
        amount,
        received: Some(usdc(5_000)),
    })
    .unwrap();
    assert_eq!(pool.payouts[&id].plan, Some(plan_for(usdc(1))));
    assert_eq!(pool.ledger.owed, amount);
}

/// The planned amount with any other premium, the user's fixed one included, is refused.
#[test]
fn a_planned_amount_with_another_premium_is_refused() {
    let mut pool = with_base(funded(usdc(1_000)), usdc(5_000), 0x5a, 1);
    let token = candid::Principal::from_slice(&[0x31; 10]);
    let id = open_planned(
        &mut pool,
        planned_swap(token, usdc(1), usdc(1)),
        plan_for(usdc(1)),
    )
    .unwrap();
    for premium in [0_u64, 1_000, 4_999_999, 5_000_001] {
        let amount = usdc(5_000)
            .checked_sub(TokenAmount::from(premium))
            .and_then(|left| left.checked_sub(TokenAmount::from(50_000_u32)))
            .unwrap();
        assert!(matches!(
            pool.step(PoolStep::PayoutAmountKnown {
                id,
                amount,
                received: Some(usdc(5_000)),
            }),
            Err(PoolStepError::Terms(_))
        ));
    }
    // and a premium the opening named is refused there
    let swap = IcpSwapTerms {
        premium: usdc(1),
        ..planned_swap(token, usdc(2), usdc(1))
    };
    assert!(matches!(
        open_planned(&mut pool, swap, plan_for(usdc(1))),
        Err(PoolStepError::Terms(_))
    ));
}

/// A planned swap opens only while the Base pocket can pay it behind an empty queue: the
/// plan's amount and 5%, and a ckUSDC swap's own payout too; its plan, slippage and planned
/// amount come together and agree.
#[test]
fn a_planned_open_needs_the_base_pocket_and_five_percent() {
    let token = candid::Principal::from_slice(&[0x31; 10]);
    let mut pool = with_base(funded(usdc(1_000)), usdc(100), 0x5b, 1);
    let free = pool.ledger.pockets.base.free;
    assert_eq!(free, usdc(100));
    // 100 USDC and 5% is 105: refused; 95.23 and 5% fits
    assert!(matches!(
        open_planned(
            &mut pool,
            planned_swap(token, usdc(1), usdc(100)),
            plan_for(usdc(100))
        ),
        Err(PoolStepError::BaseShort { .. })
    ));
    let fits = TokenAmount::from(95_238_095_u64);
    open_planned(
        &mut pool,
        planned_swap(token, usdc(1), fits),
        plan_for(fits),
    )
    .unwrap();
    // a plan whose order spends another amount, a slippage above 500 bps, a plan alone
    let mut pool = with_base(funded(usdc(1_000)), usdc(5_000), 0x5c, 1);
    assert!(matches!(
        open_planned(
            &mut pool,
            planned_swap(token, usdc(1), usdc(10)),
            plan_for(usdc(11))
        ),
        Err(PoolStepError::Terms(_))
    ));
    let steep = IcpSwapTerms {
        slippage_bps: Some(501),
        ..planned_swap(token, usdc(1), usdc(10))
    };
    assert!(matches!(
        open_planned(&mut pool, steep, plan_for(usdc(10))),
        Err(PoolStepError::Terms(_))
    ));
    let unplanned = IcpSwapTerms {
        planned: None,
        slippage_bps: None,
        premium: usdc(1),
        ..planned_swap(token, usdc(10), usdc(10))
    };
    assert_eq!(
        open_planned(&mut pool, unplanned, plan_for(usdc(10))),
        Err(PoolStepError::Mismatch)
    );
    // ckUSDC: its own payout (100 less 10 bps and the fee) must be covered too, and its
    // least ckUSDC is all of it
    let ckusdc = super::super::tests::settings().ckusdc_ledger;
    let mut pool = with_base(
        funded(usdc(1_000)),
        TokenAmount::from(99_000_000_u64),
        0x5d,
        1,
    );
    assert!(matches!(
        open_planned(
            &mut pool,
            planned_swap(ckusdc, usdc(99), usdc(90)),
            plan_for(usdc(90))
        ),
        Err(PoolStepError::Terms(_))
    ));
    let ckusdc_swap = planned_swap(ckusdc, usdc(100), usdc(90));
    // 90 and 5% is 94.5, under the 99 free; but the payout is 99.85
    let pool_short = open_planned(&mut pool, ckusdc_swap, plan_for(usdc(90)));
    assert!(
        matches!(pool_short, Err(PoolStepError::BaseShort { need, .. }) if need == TokenAmount::from(99_850_000_u64))
    );
}

fn resolved(id: u64, resolution: crate::pool::payout::PayoutResolution) -> PoolStep {
    PoolStep::PayoutResolved {
        id,
        resolution,
        refund_at: Some(Timestamp::from_nanos(5)),
    }
}

fn stopped(id: u64) -> PoolStep {
    PoolStep::PayoutStopped {
        id,
        why: "ALERT: a human's".to_string(),
    }
}

/// A payout signed, reverted, then stopped for a human (its order ran after all): a
/// withdrawal of 1,000 USDC, or an ICP swap of 100 ckUSDC.
fn stopped_after_its_revert(pool: &mut Pool, withdrawing: bool) -> u64 {
    let (kind, undo, amount, received) = if withdrawing {
        let (kind, undo) = withdrawal(pool, usdc(1_000), 31);
        let PayoutKind::Withdrawal(w) = kind else {
            unreachable!()
        };
        (kind, undo, w.payout_amount().unwrap(), None)
    } else {
        let (kind, undo) = icp_swap(usdc(100));
        (
            kind,
            undo,
            TokenAmount::from(100_000_000_u64 - 60_000),
            Some(usdc(100)),
        )
    };
    let id = open(pool, kind, undo);
    pool.step(PoolStep::PayoutAmountKnown {
        id,
        amount,
        received,
    })
    .unwrap();
    pool.step(PoolStep::PayoutWaiting { id }).unwrap();
    pool.step(signed(id, order(30))).unwrap();
    pool.step(PoolStep::PayoutReverted { id, block: None })
        .unwrap();
    pool.step(stopped(id)).unwrap();
    id
}

/// The review's finding (`resolve_payout`, Jeff 2026-10-23): a payout stopped for a human
/// was locked for good. A controller marks it paid (its order ran after all): its lines end
/// exactly where a paid payout's do, once.
#[test]
fn a_stopped_payout_marked_paid_books_its_paid_line_once() {
    use crate::pool::payout::PayoutResolution::MarkPaid;
    for withdrawing in [false, true] {
        let mut pool = with_base(funded(usdc(1_000)), usdc(5_000), 0x5e, 1);
        let id = stopped_after_its_revert(&mut pool, withdrawing);
        // where the same payout paid at once would end
        let mut straight = with_base(funded(usdc(1_000)), usdc(5_000), 0x5e, 1);
        let other = {
            let (kind, undo, amount, received) = if withdrawing {
                let (kind, undo) = withdrawal(&straight, usdc(1_000), 31);
                let PayoutKind::Withdrawal(w) = kind else {
                    unreachable!()
                };
                (kind, undo, w.payout_amount().unwrap(), None)
            } else {
                let (kind, undo) = icp_swap(usdc(100));
                (
                    kind,
                    undo,
                    TokenAmount::from(100_000_000_u64 - 60_000),
                    Some(usdc(100)),
                )
            };
            let id = open(&mut straight, kind, undo);
            straight
                .step(PoolStep::PayoutAmountKnown {
                    id,
                    amount,
                    received,
                })
                .unwrap();
            straight.step(PoolStep::PayoutWaiting { id }).unwrap();
            straight.step(signed(id, order(30))).unwrap();
            straight.step(paid(id, None)).unwrap();
            id
        };
        assert_eq!(other, id);
        pool.step(resolved(id, MarkPaid)).unwrap();
        assert_eq!(status(&pool, id), PayoutStatus::Paid, "{withdrawing}");
        assert_eq!(pool.ledger.pockets, straight.ledger.pockets);
        assert_eq!(pool.ledger.owed, straight.ledger.owed);
        assert_eq!(
            pool.ledger.lines().fees_base,
            straight.ledger.lines().fees_base
        );
        assert_eq!(
            pool.ledger.lines().fees_icp,
            straight.ledger.lines().fees_icp
        );
        assert!(pool.ledger.lines().open.is_empty());
        // once
        assert!(pool.step(resolved(id, MarkPaid)).is_err());
        assert!(pool
            .step(resolved(id, crate::pool::payout::PayoutResolution::Refund))
            .is_err());
    }
}

/// A controller refunds a stopped ICP swap: it is back on its undo, its refund asked next
/// under the instant the door chose; its undo then leaves every line where it was.
#[test]
fn a_stopped_icp_swap_refunded_goes_back_on_its_undo() {
    use crate::pool::payout::PayoutResolution::Refund;
    let mut pool = with_base(funded(usdc(1_000)), usdc(5_000), 0x5f, 1);
    let before = pool.ledger.clone();
    let id = stopped_after_its_revert(&mut pool, false);
    assert!(pool.ledger.lines().open.is_empty());
    pool.step(resolved(id, Refund)).unwrap();
    assert!(matches!(status(&pool, id), PayoutStatus::Undoing(_)));
    assert_eq!(pool.payouts[&id].undoing_at, Some(Timestamp::from_nanos(5)));
    assert!(pool.ledger.lines().open.contains(&id));
    assert!(pool.step(resolved(id, Refund)).is_err());
    pool.step(PoolStep::PayoutUndone {
        id,
        why: "refunded by a controller".to_string(),
        block: Some(16),
    })
    .unwrap();
    assert_eq!(pool.ledger.pockets, before.pockets);
    assert_eq!(pool.ledger.owed, before.owed);
    assert_eq!(pool.ledger.lines().fees_icp, TokenAmount::ZERO);
}

/// A controller refunds a stopped withdrawal: its shares are minted back with its undo.
#[test]
fn a_stopped_withdrawal_refunded_is_reminted() {
    use crate::pool::payout::PayoutResolution::Refund;
    let mut pool = with_base(funded(usdc(1_000)), usdc(5_000), 0x60, 1);
    let before = pool.ledger.clone();
    let shares = pool.shares_of_evm(staker());
    let id = stopped_after_its_revert(&mut pool, true);
    pool.step(resolved(id, Refund)).unwrap();
    pool.step(PoolStep::PayoutUndone {
        id,
        why: "refunded by a controller".to_string(),
        block: None,
    })
    .unwrap();
    assert_eq!(pool.ledger.pockets, before.pockets);
    assert_eq!(pool.ledger.owed, before.owed);
    assert_eq!(pool.ledger.total_shares, before.total_shares);
    assert_eq!(pool.shares_of_evm(staker()), shares);
}

/// Only a payout stopped for a human is resolved; only one whose order was signed, and whose
/// Eco intent was not read unfilled, can be marked paid.
#[test]
fn only_a_stopped_payout_is_resolved_and_only_a_signed_one_marked_paid() {
    use crate::pool::payout::PayoutResolution::{MarkPaid, Refund};
    let mut pool = with_base(funded(usdc(1_000)), usdc(5_000), 0x61, 1);
    // waiting, not stopped
    let (kind, undo) = icp_swap(usdc(100));
    let id = open(&mut pool, kind, undo);
    for resolution in [Refund, MarkPaid] {
        assert!(matches!(
            pool.step(resolved(id, resolution)),
            Err(PoolStepError::PayoutNotFrom { .. })
        ));
    }
    // stopped before its amount was known: never signed, nothing paid
    pool.step(stopped(id)).unwrap();
    assert!(matches!(
        pool.step(resolved(id, MarkPaid)),
        Err(PoolStepError::Terms(_))
    ));
    pool.step(resolved(id, Refund)).unwrap();
    // an unfilled Eco payout whose refund stopped: its money was never delivered
    let id = delivering(&mut pool, false);
    pool.step(unfilled(id)).unwrap();
    pool.step(PoolStep::PayoutUndoing {
        id,
        why: "unfilled".to_string(),
    })
    .unwrap();
    pool.step(stopped(id)).unwrap();
    assert!(matches!(
        pool.step(resolved(id, MarkPaid)),
        Err(PoolStepError::Terms(_))
    ));
}

/// CR-08's payouts held for a human before they started are deposits paid on ICP, which
/// `resolve_payout` never reaches. A payout from the Base pocket is stopped only once it
/// started (its pull out, its legs, its undo, its revert), never queued, known or waiting;
/// one stopped before its amount was known booked nothing, and its refund leaves every line
/// exactly as it was.
#[test]
fn a_base_payout_stopped_before_its_amount_is_known_resolves_with_every_line_as_it_was() {
    use crate::pool::payout::PayoutResolution::Refund;
    let mut pool = with_base(funded(usdc(1_000)), usdc(5_000), 0x62, 1);
    // never stopped while known, queued or waiting
    let (kind, undo) = withdrawal(&pool, usdc(1_000), 41);
    let PayoutKind::Withdrawal(w) = kind else {
        unreachable!()
    };
    let known = open(&mut pool, kind, undo);
    pool.step(PoolStep::PayoutAmountKnown {
        id: known,
        amount: w.payout_amount().unwrap(),
        received: None,
    })
    .unwrap();
    assert!(pool.step(stopped(known)).is_err());
    pool.step(PoolStep::PayoutQueued { id: known }).unwrap();
    assert!(pool.step(stopped(known)).is_err());
    pool.step(PoolStep::PayoutUndone {
        id: known,
        why: "past its deadline".to_string(),
        block: None,
    })
    .unwrap();
    // an ICP swap stopped with its pull's outcome unknown: nothing booked
    let before = pool.ledger.clone();
    let (kind, undo) = icp_swap(usdc(100));
    let id = open(&mut pool, kind, undo);
    pool.step(stopped(id)).unwrap();
    pool.step(resolved(id, Refund)).unwrap();
    pool.step(PoolStep::PayoutUndone {
        id,
        why: crate::pool::payout::REFUNDED.to_string(),
        block: Some(17),
    })
    .unwrap();
    assert_eq!(pool.ledger.pockets, before.pockets);
    assert_eq!(pool.ledger.owed, before.owed);
    assert_eq!(pool.ledger.lines().queue_len, 0);
    assert_eq!(pool.ledger.lines().fees_icp, before.lines().fees_icp);
}

/// The review (2026-10-23): a planned opening refused for the Base pocket reports the line it
/// checked, the pocket's free USDC, not what is left of it behind the queue.
#[test]
fn a_planned_open_refused_reports_the_free_line_it_checked() {
    let token = candid::Principal::from_slice(&[0x31; 10]);
    let pool = with_base(funded(usdc(1_000)), usdc(100), 0x63, 1);
    let mut pool = with_icp_for_staker(pool, usdc(3_000));
    // a withdrawal the pocket cannot cover queues
    let (kind, undo) = withdrawal(&pool, usdc(1_000), 51);
    let PayoutKind::Withdrawal(w) = kind else {
        unreachable!()
    };
    let queued = open(&mut pool, kind, undo);
    pool.step(PoolStep::PayoutAmountKnown {
        id: queued,
        amount: w.payout_amount().unwrap(),
        received: None,
    })
    .unwrap();
    pool.step(PoolStep::PayoutQueued { id: queued }).unwrap();
    assert!(pool.ledger.lines().queued > TokenAmount::ZERO);
    let free = pool.ledger.pockets.base.free;
    assert!(matches!(
        open_planned(&mut pool, planned_swap(token, usdc(1), usdc(10)), plan_for(usdc(10))),
        Err(PoolStepError::BaseShort { free: reported, .. }) if reported == free
    ));
}

fn fit_waited(id: u64) -> PoolStep {
    PoolStep::PayoutFitWaited {
        id,
        why: "the scaler's read: Unreachable".to_string(),
    }
}

/// The review (2026-10-23): a planned payout's plan is tried at most three times in all,
/// each try that waits counted in the payout; only while it waits with its plan, never
/// signed; and a try moves no line.
#[test]
fn a_plan_is_tried_at_most_three_times() {
    let token = candid::Principal::from_slice(&[0x31; 10]);
    let mut pool = with_base(funded(usdc(1_000)), usdc(5_000), 0x64, 1);
    let id = open_planned(
        &mut pool,
        planned_swap(token, usdc(1), usdc(10)),
        plan_for(usdc(10)),
    )
    .unwrap();
    // not before it waits
    assert!(pool.step(fit_waited(id)).is_err());
    let received = usdc(10);
    let amount = crate::pool::plan::payout_of(
        pool.ledger.settings.as_ref().unwrap(),
        received,
        TokenAmount::from(50_000_u32),
    )
    .unwrap();
    pool.step(PoolStep::PayoutAmountKnown {
        id,
        amount,
        received: Some(received),
    })
    .unwrap();
    pool.step(PoolStep::PayoutWaiting { id }).unwrap();
    let before = pool.ledger.clone();
    for tries in 1..=crate::pool::payout::MAX_FIT_TRIES {
        pool.step(fit_waited(id)).unwrap();
        assert_eq!(pool.payouts[&id].fit_tries, Some(tries));
    }
    assert!(pool.step(fit_waited(id)).is_err());
    assert_eq!(pool.ledger, before);
    // a payout with no plan is never tried
    let (kind, undo) = withdrawal(&pool, usdc(1_000), 61);
    let other = open(&mut pool, kind, undo);
    assert!(pool.step(fit_waited(other)).is_err());
}

/// The review (2026-10-23): a fill read too early is noted on the delivering payout only;
/// nothing moves.
#[test]
fn a_fill_read_too_early_is_noted_on_a_delivering_payout_only() {
    let mut pool = with_base(funded(usdc(1_000)), usdc(5_000), 0x65, 1);
    let id = delivering(&mut pool, false);
    let before = pool.ledger.clone();
    pool.step(PoolStep::PayoutFillEarly { id }).unwrap();
    assert_eq!(
        pool.payouts[&id].fill_early_at,
        Some(Timestamp::from_nanos(1))
    );
    assert_eq!(pool.ledger, before);
    pool.step(PoolStep::PayoutDelivered {
        id,
        block: BlockNumber::new(35),
    })
    .unwrap();
    assert!(pool.step(PoolStep::PayoutFillEarly { id }).is_err());
}
