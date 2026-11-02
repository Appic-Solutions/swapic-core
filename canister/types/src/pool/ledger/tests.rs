use super::*;
use crate::evm::EvmAddress;
use crate::hash::TxHash;
use crate::pool::{
    IcpAccount, LedgerFee, PayoutAttempt, PoolRail, PrincipalBytes, Sent, CKUSDC_LEDGER,
};

/// The maps the storage layer keeps beside the ledger, on the heap, and the steps applied the
/// way the storage layer applies them: `admits`, then every effect.
#[derive(Default)]
pub(crate) struct Pool {
    pub ledger: PoolLedger,
    pub deposits: BTreeMap<PoolRef, Registered>,
    pub shares: BTreeMap<OwnerKey, ShareLine>,
    pub credited: BTreeSet<CreditKey>,
    pub payouts: BTreeMap<u64, EvmPayout>,
    pub queue: BTreeMap<u64, u64>,
    pub nonces: BTreeSet<NonceUse>,
    pub orphans: BTreeMap<CreditKey, crate::pool::Orphan>,
}

impl PoolLookup for Pool {
    fn deposit(&self, r#ref: &PoolRef) -> Option<Registered> {
        self.deposits.get(r#ref).cloned()
    }

    fn shares_of(&self, owner: &OwnerKey) -> Shares {
        self.shares
            .get(owner)
            .map_or(Shares::ZERO, |line| line.shares)
    }

    fn is_credited(&self, key: &CreditKey) -> bool {
        self.credited.contains(key)
    }

    fn payout(&self, id: u64) -> Option<EvmPayout> {
        self.payouts.get(&id).cloned()
    }

    fn queue_head(&self) -> Option<(u64, u64)> {
        self.queue.iter().next().map(|(seq, id)| (*seq, *id))
    }

    fn nonce_used(&self, key: &NonceUse) -> bool {
        self.nonces.contains(key)
    }

    fn orphan(&self, key: &CreditKey) -> Option<crate::pool::Orphan> {
        self.orphans.get(key).cloned()
    }
}

impl Pool {
    pub fn step(&mut self, step: PoolStep) -> Result<(), PoolStepError> {
        self.ledger.admits(&step, self)?;
        let effects = self
            .ledger
            .clone()
            .apply(&step, self, Timestamp::from_nanos(1));
        self.ledger = effects.ledger;
        if let Some((r#ref, deposit)) = effects.deposit {
            self.deposits.insert(r#ref, deposit);
        }
        if let Some((key, line)) = effects.shares {
            self.shares.insert(key, line);
        }
        if let Some(key) = effects.credited {
            self.credited.insert(key);
        }
        if let Some(payout) = effects.payout {
            self.payouts.insert(payout.id, payout);
        }
        if let Some(seq) = effects.dequeued {
            self.queue.remove(&seq);
        }
        if let Some((seq, id)) = effects.queued {
            self.queue.insert(seq, id);
        }
        if let Some(key) = effects.nonce {
            self.nonces.insert(key);
        }
        if let Some((key, orphan)) = effects.orphan {
            self.orphans.insert(key, orphan);
        }
        Ok(())
    }
}

fn ckusdc() -> Principal {
    Principal::from_text(CKUSDC_LEDGER).unwrap()
}

pub(crate) fn usdc(whole: u64) -> TokenAmount {
    TokenAmount::from(whole * 1_000_000)
}

pub(crate) const FEE: u64 = 10_000;

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

pub(crate) fn payer() -> EvmAddress {
    "0xf00EBA8E5Fd231b526ADFAAa68B4d0ede72A89AC"
        .parse()
        .unwrap()
}

pub(crate) fn recipient(byte: u8) -> IcpAccount {
    IcpAccount {
        owner: Principal::from_slice(&[byte; 10]),
        subaccount: None,
    }
}

pub(crate) fn swap(byte: u8, min_usdc: TokenAmount, premium: TokenAmount) -> Registration {
    Registration {
        kind: DepositKind::Swap {
            recipient: recipient(byte),
            token_out: ckusdc(),
            min_out: 1,
        },
        min_usdc,
        premium,
        move_fee: TokenAmount::ZERO,
        src_chain: ChainId::ARBITRUM,
        src_payer: payer(),
        rail: PoolRail::Cctp,
        expires_at: Timestamp::from_nanos(1_000),
        salt: [byte; 32],
        icp_fees: Some(TokenAmount::from(FEE)),
        least_usdc: TokenAmount::ZERO,
    }
}

fn stake(byte: u8, min_usdc: TokenAmount, move_fee: TokenAmount) -> Registration {
    Registration {
        kind: DepositKind::Stake {
            owner: PoolAccount::Icp(recipient(byte)),
        },
        min_usdc,
        premium: TokenAmount::ZERO,
        move_fee,
        src_chain: ChainId::BASE,
        src_payer: payer(),
        rail: PoolRail::SameChain,
        expires_at: Timestamp::from_nanos(1_000),
        salt: [byte; 32],
        icp_fees: None,
        least_usdc: TokenAmount::ZERO,
    }
}

pub(crate) fn credit(log: u64) -> CreditKey {
    CreditKey {
        chain: ChainId::BASE,
        tx: TxHash::new([0xab; 32]),
        log_index: log,
    }
}

pub(crate) fn register(pool: &mut Pool, terms: Registration) -> PoolRef {
    let r#ref = terms.pool_ref().unwrap();
    pool.step(PoolStep::Registered {
        r#ref,
        deposit: terms,
    })
    .unwrap();
    r#ref
}

/// A pool with settings and `icp` of ckUSDC staked on ICP by `recipient(200)`.
pub(crate) fn funded(icp: TokenAmount) -> Pool {
    let mut pool = Pool::default();
    pool.step(PoolStep::SettingsSet {
        settings: settings(),
    })
    .unwrap();
    let id = PoolOpId::new([0xee; 32]);
    let owner = PoolAccount::Icp(recipient(200));
    pool.step(PoolStep::StakeStarted {
        id,
        owner,
        from: recipient(200),
        amount: icp,
        created_at: Timestamp::from_nanos(1),
        move_fee: TokenAmount::ZERO,
    })
    .unwrap();
    let shares = pool.ledger.mint_now(icp).unwrap();
    pool.step(PoolStep::StakedIcp {
        id,
        owner,
        amount: icp,
        move_fee: TokenAmount::ZERO,
        shares,
        block: 1,
    })
    .unwrap();
    pool
}

/// P11d (2026-10-21): the terms are checked at the arrival, on what arrived; the ICP fees
/// at least the path's own ckUSDC fees. No cap on an EVM to ICP deposit (2026-10-23): the
/// per-swap cap is the quoter's own.
#[test]
fn an_arrival_below_its_icp_fees_is_refused_and_none_above_a_cap() {
    let pool = funded(usdc(5_000));
    let good = swap(1, usdc(100), usdc(1));
    assert_eq!(check_arrival(&pool.ledger, &good), Ok(()));
    // above the settings' per-swap cap: taken
    assert_eq!(
        check_arrival(&pool.ledger, &swap(1, usdc(1_001), usdc(2))),
        Ok(())
    );
    // the premium at arrival (P11e): at least 90% of 10 bps of what arrived, $100 here,
    // so 90,000 at the least
    assert_eq!(
        check_arrival(&pool.ledger, &swap(1, usdc(100), TokenAmount::ONE)),
        Err(RegisterRefusal::PremiumTooLow {
            least: TokenAmount::from(90_000_u64)
        })
    );
    assert_eq!(
        check_arrival(
            &pool.ledger,
            &swap(1, usdc(100), TokenAmount::from(89_999_u64))
        ),
        Err(RegisterRefusal::PremiumTooLow {
            least: TokenAmount::from(90_000_u64)
        })
    );
    assert_eq!(
        check_arrival(
            &pool.ledger,
            &swap(1, usdc(100), TokenAmount::from(90_000_u64))
        ),
        Ok(())
    );
    // $1 at 10 bps is a share of 1,000: 900 is its 90%
    assert_eq!(
        check_arrival(&pool.ledger, &swap(1, usdc(1), TokenAmount::from(900_u64))),
        Ok(())
    );
    // the ICP fees below the path's own: one ckUSDC fee for a ckUSDC payout
    let cheap = Registration {
        icp_fees: Some(TokenAmount::from(FEE - 1)),
        ..good.clone()
    };
    assert_eq!(
        check_arrival(&pool.ledger, &cheap),
        Err(RegisterRefusal::NotAllowed(format!(
            "the ICP fees must be at least {FEE}"
        )))
    );
    // a stake that would take the pool past its TVL cap
    assert_eq!(
        check_arrival(&pool.ledger, &stake(2, usdc(5_001), TokenAmount::ZERO)),
        Err(RegisterRefusal::AboveTvlCap {
            cap: usdc(10_000),
            would_be: usdc(10_001)
        })
    );
    assert_eq!(
        check_arrival(&pool.ledger, &stake(2, usdc(5_000), TokenAmount::ZERO)),
        Ok(())
    );
    // a ckUSDC payout of arrived - premium - icp_fees below the recipient's own minimum
    let mut short = swap(1, usdc(100), usdc(1));
    short.kind = DepositKind::Swap {
        recipient: recipient(1),
        token_out: ckusdc(),
        min_out: 99_000_000 - u128::from(FEE) + 1,
    };
    assert_eq!(
        check_arrival(&pool.ledger, &short),
        Err(RegisterRefusal::NotAllowed(
            "below the recipient's minimum".to_string()
        ))
    );
    // a token the settings do not list
    let other = Principal::from_slice(&[9; 10]);
    let mut unlisted = swap(1, usdc(100), usdc(1));
    unlisted.kind = DepositKind::Swap {
        recipient: recipient(1),
        token_out: other,
        min_out: 1,
    };
    assert_eq!(
        check_arrival(&pool.ledger, &unlisted),
        Err(RegisterRefusal::TokenNotListed(other))
    );
    // closed for swaps; a stake still arrives
    let mut closed = funded(usdc(5_000));
    closed.ledger.settings.as_mut().unwrap().open_for_swaps = false;
    assert_eq!(
        check_arrival(&closed.ledger, &good),
        Err(RegisterRefusal::ClosedForSwaps)
    );
    assert_eq!(
        check_arrival(&closed.ledger, &stake(3, usdc(10), TokenAmount::ZERO)),
        Ok(())
    );
    // nothing is held, so a payout the free ckUSDC cannot cover yet still arrives (queued)
    assert_eq!(
        check_arrival(&funded(usdc(10)).ledger, &swap(1, usdc(100), usdc(1))),
        Ok(())
    );
}

#[test]
fn the_fees_the_ref_commits_are_checked() {
    let pool = funded(usdc(5_000));
    let terms = swap(1, usdc(100), usdc(1));
    // a swap with a rebalancing share the ref does not commit
    let both = Registration {
        move_fee: TokenAmount::ONE,
        ..terms.clone()
    };
    assert!(matches!(
        check_arrival(&pool.ledger, &both),
        Err(RegisterRefusal::NotAllowed(_))
    ));
    // the premium and the ICP fees take all that arrived
    let all = Registration {
        premium: usdc(100).checked_sub(TokenAmount::from(FEE)).unwrap(),
        ..terms.clone()
    };
    assert!(matches!(
        check_arrival(&pool.ledger, &all),
        Err(RegisterRefusal::NotAllowed(_))
    ));
    // a stake with ICP fees
    let fees = Registration {
        icp_fees: Some(TokenAmount::ONE),
        ..stake(2, usdc(10), TokenAmount::ZERO)
    };
    assert!(matches!(
        check_arrival(&pool.ledger, &fees),
        Err(RegisterRefusal::NotAllowed(_))
    ));
}

/// Jeff's hold rule: a registration, swap or stake, holds nothing of the pockets nor of the
/// TVL room, and its expiry releases nothing.
#[test]
fn a_registration_holds_nothing() {
    let mut pool = funded(usdc(1_000));
    let r#ref = register(&mut pool, swap(1, usdc(100), usdc(1)));
    assert_eq!(pool.ledger.pockets.icp.reserved, TokenAmount::ZERO);
    assert_eq!(pool.ledger.pockets.icp.free, usdc(1_000));
    assert_eq!(pool.ledger.open_of(&PoolAccount::Evm(payer()).key()), 1);
    assert!(pool.ledger.open.contains(&r#ref));
    // registered twice
    assert_eq!(
        pool.step(PoolStep::Registered {
            r#ref,
            deposit: swap(1, usdc(100), usdc(1))
        }),
        Err(PoolStepError::Known(r#ref))
    );
    pool.step(PoolStep::Expired { r#ref }).unwrap();
    assert_eq!(pool.ledger.pockets.icp.free, usdc(1_000));
    assert_eq!(pool.ledger.open_of(&PoolAccount::Evm(payer()).key()), 0);
    assert!(pool.ledger.open.is_empty());
    assert_eq!(pool.deposits[&r#ref].status, DepositStatus::Expired);
    assert!(pool.step(PoolStep::Expired { r#ref }).is_err());

    let staked = register(&mut pool, stake(2, usdc(50), TokenAmount::ZERO));
    assert_eq!(pool.ledger.stake_room, TokenAmount::ZERO);
    pool.step(PoolStep::Expired { r#ref: staked }).unwrap();
    assert_eq!(pool.ledger.tvl_measure(), usdc(1_000));
}

/// Review Focus 1 at the fold: one transfer log is credited once, whatever names it.
#[test]
fn a_transfer_is_credited_once() {
    let mut pool = funded(usdc(1_000));
    let first = register(&mut pool, swap(1, usdc(100), usdc(1)));
    let second = register(&mut pool, swap(2, usdc(100), usdc(1)));
    let arrived = |r#ref| PoolStep::Arrived {
        r#ref,
        credit: credit(7),
        amount: usdc(100),
        block: BlockNumber::new(10),
    };
    pool.step(arrived(first)).unwrap();
    assert_eq!(
        pool.step(arrived(second)),
        Err(PoolStepError::AlreadyCredited(credit(7)))
    );
    assert_eq!(
        pool.step(PoolStep::Unattributed {
            credit: credit(7),
            amount: usdc(100),
            src_payer: None,
            why: "again".into(),
            block: BlockNumber::new(10),
            r#ref: None,
            awaiting_until: None,
        }),
        Err(PoolStepError::AlreadyCredited(credit(7)))
    );
    // below its minimum, an arrival is no credit
    assert!(matches!(
        pool.step(PoolStep::Arrived {
            r#ref: second,
            credit: credit(8),
            amount: usdc(99),
            block: BlockNumber::new(10),
        }),
        Err(PoolStepError::BelowMinimum { .. })
    ));
}

/// Review Focus 4 at the fold: a late arrival is recorded unattributed with its payer,
/// closes its registration, and is no pool asset.
#[test]
fn a_late_arrival_is_unattributed_and_closes_its_registration() {
    let mut pool = funded(usdc(1_000));
    let r#ref = register(&mut pool, swap(1, usdc(100), usdc(1)));
    let before = pool.ledger.total_assets();
    pool.step(PoolStep::Unattributed {
        credit: credit(1),
        amount: usdc(100),
        src_payer: Some(payer()),
        why: "arrived past its grace".into(),
        block: BlockNumber::new(10),
        r#ref: Some(r#ref),
        awaiting_until: None,
    })
    .unwrap();
    assert_eq!(pool.deposits[&r#ref].status, DepositStatus::Unattributed);
    assert_eq!(pool.ledger.pockets.icp.reserved, TokenAmount::ZERO);
    assert_eq!(pool.ledger.unattributed, usdc(100));
    assert_eq!(
        pool.ledger.total_assets(),
        before,
        "an orphan is no one's stake"
    );
    assert!(pool.ledger.open.is_empty());
    assert_eq!(pool.ledger.base_held(), usdc(100));
}

#[test]
fn a_donation_moves_no_share_price() {
    let mut pool = funded(usdc(1_000));
    let staker = pool.shares_of(&PoolAccount::Icp(recipient(200)).key());
    let value = |pool: &Pool| {
        Shares::value_of(staker, pool.ledger.total_assets(), pool.ledger.total_shares)
    };
    let before = value(&pool);
    // a transfer to the canister's address that proves nothing
    pool.step(PoolStep::Unattributed {
        credit: credit(3),
        amount: usdc(500),
        src_payer: None,
        why: "no proof".into(),
        block: BlockNumber::new(10),
        r#ref: None,
        awaiting_until: None,
    })
    .unwrap();
    assert_eq!(value(&pool), before);
    // and A is read off the ledger lines, never a balance
    assert_eq!(pool.ledger.total_assets(), usdc(1_000));
}

/// A swap pays `arrived - premium`; the premium stays with the stakers; the payout's fee
/// leaves the ICP pocket; nothing is counted twice between the arrival and the payout.
#[test]
fn a_swap_moves_the_pockets_and_keeps_the_premium() {
    let mut pool = funded(usdc(1_000));
    let r#ref = register(&mut pool, swap(1, usdc(100), usdc(1)));
    pool.step(PoolStep::Arrived {
        r#ref,
        credit: credit(1),
        amount: usdc(100),
        block: BlockNumber::new(10),
    })
    .unwrap();
    // in the pockets twice (Base arrival, ICP reservation), owed once: the user's budget,
    // out of which its ICP fees are paid (P11d)
    let held = usdc(99);
    assert_eq!(pool.ledger.owed, held);
    assert_eq!(pool.ledger.total_assets(), usdc(1_001));
    assert!(pool.ledger.paying.contains(&r#ref));
    let attempt = PayoutAttempt {
        created_at: Timestamp::from_nanos(5),
        memo: r#ref.into_bytes(),
        path: PayoutPath::Ckusdc,
        amount: None,
        token: None,
    };
    pool.step(PoolStep::PayoutStarted { r#ref, attempt })
        .unwrap();
    assert_eq!(pool.deposits[&r#ref].status, DepositStatus::Paying);
    pool.step(PoolStep::Paid {
        r#ref,
        token: ckusdc(),
        amount: usdc(99).checked_sub(TokenAmount::from(FEE)).unwrap(),
        block: 42,
        spent: usdc(99),
    })
    .unwrap();
    assert_eq!(pool.deposits[&r#ref].status, DepositStatus::Paid);
    assert_eq!(pool.ledger.owed, TokenAmount::ZERO);
    assert_eq!(pool.ledger.pockets.icp.reserved, TokenAmount::ZERO);
    // 1000 - 99 on ICP (the fee the user's), 100 on Base: the whole premium earned
    assert_eq!(pool.ledger.pockets.icp.free, usdc(901));
    assert_eq!(pool.ledger.pockets.base.free, usdc(100));
    assert_eq!(pool.ledger.total_assets(), usdc(1_001));
    assert!(pool.ledger.paying.is_empty());
}

/// A stake mints on what arrived less its rebalancing share, at the price before it.
#[test]
fn a_stake_mints_at_the_price_before_it() {
    let mut pool = funded(usdc(1_000));
    let owner = PoolAccount::Icp(recipient(2));
    let r#ref = register(&mut pool, stake(2, usdc(100), usdc(1)));
    pool.step(PoolStep::Arrived {
        r#ref,
        credit: credit(1),
        amount: usdc(100),
        block: BlockNumber::new(10),
    })
    .unwrap();
    assert_eq!(
        pool.ledger.total_assets(),
        usdc(1_000),
        "its own arrival is not priced in"
    );
    let shares = pool.ledger.mint_now(usdc(99)).unwrap();
    assert_eq!(
        pool.step(PoolStep::Minted {
            r#ref,
            owner,
            shares: Shares::new(shares.get() + 1)
        }),
        Err(PoolStepError::WrongMint {
            given: Shares::new(shares.get() + 1),
            expected: Some(shares)
        })
    );
    pool.step(PoolStep::Minted {
        r#ref,
        owner,
        shares,
    })
    .unwrap();
    assert_eq!(pool.shares_of(&owner.key()), shares);
    assert_eq!(pool.deposits[&r#ref].minted, Some(shares));
    // the move fee is Swapic's, on the Base fee line, never the stakers'
    assert_eq!(pool.ledger.total_assets(), usdc(1_099));
    assert_eq!(pool.ledger.lines().fees_base, usdc(1));
    assert_eq!(
        pool.ledger.base_held(),
        usdc(100),
        "all that arrived is at the address"
    );
    // worth what came in less the move fee
    let value = Shares::value_of(shares, pool.ledger.total_assets(), pool.ledger.total_shares);
    assert!(value >= usdc(99).checked_sub(TokenAmount::ONE).unwrap());
    assert!(value <= usdc(99));
}

/// Review Focus 3, through the fold (and the coordinator's review): what a pool swap owes is
/// no stakers' asset. A withdrawal while a swap's payout is unpaid is paid exactly its fair
/// value (what was staked, and its part of the premium less the payout's fee), never the
/// arrival counted twice; and the same value after the payout is recorded.
#[test]
fn a_withdrawal_during_an_unpaid_payout_takes_no_more_than_its_value() {
    let fee = TokenAmount::from(FEE);
    // A staked 1000 (funded), B stakes 500 on ICP
    let mut pool = funded(usdc(1_000));
    let a = recipient(200);
    let b = recipient(201);
    let b_id = PoolOpId::new([0x66; 32]);
    pool.step(PoolStep::StakeStarted {
        id: b_id,
        owner: PoolAccount::Icp(b),
        from: b,
        amount: usdc(500),
        created_at: Timestamp::from_nanos(1),
        move_fee: TokenAmount::ZERO,
    })
    .unwrap();
    let b_shares = pool.ledger.mint_now(usdc(500)).unwrap();
    pool.step(PoolStep::StakedIcp {
        id: b_id,
        owner: PoolAccount::Icp(b),
        amount: usdc(500),
        move_fee: TokenAmount::ZERO,
        shares: b_shares,
        block: 2,
    })
    .unwrap();
    // a swap of 100 with a premium of 1 arrives, unpaid
    let r#ref = register(&mut pool, swap(1, usdc(100), usdc(1)));
    pool.step(PoolStep::Arrived {
        r#ref,
        credit: credit(1),
        amount: usdc(100),
        block: BlockNumber::new(10),
    })
    .unwrap();
    // the stakers own 1500 and the premium (the payout's fee is the user's), never the
    // arrival
    let fair = usdc(1_501);
    assert_eq!(pool.ledger.total_assets(), fair);
    let value = |pool: &Pool, owner: IcpAccount| {
        Shares::value_of(
            pool.shares_of(&PoolAccount::Icp(owner).key()),
            pool.ledger.total_assets(),
            pool.ledger.total_shares,
        )
    };
    // B's third of it, rounded down
    let b_fair = fair
        .checked_mul(500_u32)
        .unwrap()
        .checked_div_floor(1_500_u32)
        .unwrap();
    assert_eq!(value(&pool, b), b_fair);
    let started = |id: u8, owner: IcpAccount, usdc_out: TokenAmount, burned: Shares| {
        PoolStep::WithdrawStarted {
            id: PoolOpId::new([id; 32]),
            owner,
            burned,
            usdc: usdc_out,
            to: owner,
            token_out: ckusdc(),
            min_out: 0,
            attempt: PayoutAttempt {
                created_at: Timestamp::from_nanos(9),
                memo: [id; 32],
                path: PayoutPath::Ckusdc,
                amount: None,
                token: None,
            },
            move_fee: TokenAmount::ZERO,
        }
    };
    // a cent more than B's value burns more than B owns
    let too_much = b_fair.checked_add(TokenAmount::ONE).unwrap();
    let burn_too_much = Shares::burn_for(
        too_much,
        pool.ledger.total_assets(),
        pool.ledger.total_shares,
    )
    .unwrap();
    assert!(burn_too_much > pool.shares_of(&PoolAccount::Icp(b).key()));
    assert!(matches!(
        pool.step(started(0x77, b, too_much, burn_too_much)),
        Err(PoolStepError::ShortShares { .. })
    ));
    // too few shares for what is asked
    assert!(matches!(
        pool.step(started(0x77, b, b_fair, Shares::new(b_shares.get() / 3))),
        Err(PoolStepError::BurnTooSmall { .. })
    ));
    // B withdraws exactly its value
    let burned =
        Shares::burn_for(b_fair, pool.ledger.total_assets(), pool.ledger.total_shares).unwrap();
    assert!(burned <= b_shares);
    pool.step(started(0x77, b, b_fair, burned)).unwrap();
    pool.step(PoolStep::WithdrawnIcp {
        id: PoolOpId::new([0x77; 32]),
        owner: b,
        burned,
        usdc: b_fair,
        token: ckusdc(),
        paid: b_fair.checked_sub(fee).unwrap(),
        block: 3,
        spent: b_fair,
    })
    .unwrap();
    // A's value did not move by B's leaving, and is the rest of the fair value
    let a_before_paid = value(&pool, a);
    assert!(
        a_before_paid
            >= fair
                .checked_sub(b_fair)
                .unwrap()
                .checked_sub(TokenAmount::ONE)
                .unwrap()
    );
    // the swap's payout recorded: nothing moves for the stakers
    pool.step(PoolStep::PayoutStarted {
        r#ref,
        attempt: PayoutAttempt {
            created_at: Timestamp::from_nanos(10),
            memo: r#ref.into_bytes(),
            path: PayoutPath::Ckusdc,
            amount: None,
            token: None,
        },
    })
    .unwrap();
    pool.step(PoolStep::Paid {
        r#ref,
        token: ckusdc(),
        amount: usdc(99).checked_sub(fee).unwrap(),
        block: 4,
        spent: usdc(99),
    })
    .unwrap();
    assert_eq!(value(&pool, a), a_before_paid, "the same after Paid");
    assert_eq!(pool.ledger.owed, TokenAmount::ZERO);
    assert!(pool.ledger.pending.is_empty());
}

/// A payout that keeps failing stops for a human at the tenth failure.
#[test]
fn a_payout_stops_after_ten_failures() {
    let mut pool = funded(usdc(1_000));
    let r#ref = register(&mut pool, swap(1, usdc(100), usdc(1)));
    pool.step(PoolStep::Arrived {
        r#ref,
        credit: credit(1),
        amount: usdc(100),
        block: BlockNumber::new(10),
    })
    .unwrap();
    // started: the hold is taken
    pool.step(PoolStep::PayoutStarted {
        r#ref,
        attempt: PayoutAttempt {
            created_at: Timestamp::from_nanos(5),
            memo: r#ref.into_bytes(),
            path: PayoutPath::Ckusdc,
            amount: None,
            token: None,
        },
    })
    .unwrap();
    for n in 1..=MAX_PAYOUT_ATTEMPTS {
        pool.step(PoolStep::AttemptFailed {
            r#ref,
            why: format!("try {n}"),
        })
        .unwrap();
    }
    assert_eq!(
        pool.deposits[&r#ref].status,
        DepositStatus::PayoutFailed("try 10".into())
    );
    // stopped, and still between its start and its payment for the ICP check
    assert!(pool.ledger.paying.contains(&r#ref));
    assert!(pool
        .step(PoolStep::AttemptFailed {
            r#ref,
            why: "again".into()
        })
        .is_err());
    // the money is still owed and still held
    assert_eq!(pool.ledger.owed, usdc(99));
    assert_eq!(pool.ledger.pockets.icp.reserved, usdc(99));
}

/// The ICPSwap leg: declined once, then it may only fall back; once begun it never falls
/// back, and answers once.
#[test]
fn an_icpswap_leg_falls_back_only_before_its_call() {
    let mut pool = funded(usdc(1_000));
    let r#ref = register(&mut pool, swap(1, usdc(100), usdc(1)));
    pool.step(PoolStep::Arrived {
        r#ref,
        credit: credit(1),
        amount: usdc(100),
        block: BlockNumber::new(10),
    })
    .unwrap();
    let pool_principal = Principal::from_slice(&[5; 10]);
    pool.step(PoolStep::PayoutStarted {
        r#ref,
        attempt: PayoutAttempt {
            created_at: Timestamp::from_nanos(5),
            memo: r#ref.into_bytes(),
            path: PayoutPath::Icpswap {
                pool: pool_principal,
            },
            amount: None,
            token: None,
        },
    })
    .unwrap();
    let declined = || PoolStep::SwapDeclined {
        r#ref,
        why: "below".into(),
        leg: None,
    };
    pool.step(declined()).unwrap();
    assert!(pool.step(declined()).is_err());
    pool.step(PoolStep::SwapBegun {
        r#ref,
        before: None,
        leg: None,
        amount_in: None,
    })
    .unwrap();
    assert!(pool
        .step(PoolStep::FellBack {
            r#ref,
            why: "late".into(),
            amount: None,
            token: None,
        })
        .is_err());
    pool.step(PoolStep::Swapped {
        r#ref,
        out: TokenAmount::from(5_u8),
        send: None,
        leg: None,
    })
    .unwrap();
    assert!(pool
        .step(PoolStep::Swapped {
            r#ref,
            out: TokenAmount::from(5_u8),
            send: None,
            leg: None,
        })
        .is_err());

    // another deposit: not begun, it falls back to ckUSDC with the same dedup pair
    let other = register(&mut pool, swap(2, usdc(100), usdc(1)));
    pool.step(PoolStep::Arrived {
        r#ref: other,
        credit: credit(2),
        amount: usdc(100),
        block: BlockNumber::new(10),
    })
    .unwrap();
    let attempt = PayoutAttempt {
        created_at: Timestamp::from_nanos(6),
        memo: other.into_bytes(),
        path: PayoutPath::Icpswap {
            pool: pool_principal,
        },
        amount: None,
        token: None,
    };
    pool.step(PoolStep::PayoutStarted {
        r#ref: other,
        attempt,
    })
    .unwrap();
    pool.step(PoolStep::FellBack {
        r#ref: other,
        why: "below the minimum twice".into(),
        amount: None,
        token: None,
    })
    .unwrap();
    let payout = &pool.deposits[&other].payout;
    assert_eq!(
        payout.attempt,
        Some(PayoutAttempt {
            path: PayoutPath::Ckusdc,
            ..attempt
        })
    );
    assert_eq!(payout.fell_back.as_deref(), Some("below the minimum twice"));
}

/// Review fixes 4, 8 and 9: every approval is counted; a begun swap that did not happen
/// falls back only once undone, and then with the exact ckUSDC transfer it pays; a swap that
/// paid its output is never undone; the output's transfer is fixed with the output.
#[test]
fn a_begun_swap_falls_back_only_once_undone() {
    let mut pool = funded(usdc(1_000));
    let pool_principal = Principal::from_slice(&[5; 10]);
    let started = |pool: &mut Pool, byte: u8| {
        let r#ref = register(pool, swap(byte, usdc(100), usdc(1)));
        pool.step(PoolStep::Arrived {
            r#ref,
            credit: credit(u64::from(byte)),
            amount: usdc(100),
            block: BlockNumber::new(10),
        })
        .unwrap();
        pool.step(PoolStep::PayoutStarted {
            r#ref,
            attempt: PayoutAttempt {
                created_at: Timestamp::from_nanos(5),
                memo: r#ref.into_bytes(),
                path: PayoutPath::Icpswap {
                    pool: pool_principal,
                },
                amount: Some(usdc(99)),
                token: Some(PrincipalBytes(ckusdc())),
            },
        })
        .unwrap();
        r#ref
    };
    let r#ref = started(&mut pool, 1);
    let undone = |cost: u32| PoolStep::SwapUndone {
        r#ref,
        cost: TokenAmount::from(cost),
        withdrawn: usdc(99),
        leg: None,
    };
    let fell_back = || PoolStep::FellBack {
        r#ref,
        why: "the pool refused the swap".into(),
        amount: Some(usdc(99)),
        token: Some(PrincipalBytes(ckusdc())),
    };
    // nothing to undo before the swap began
    assert!(pool.step(undone(20_000)).is_err());
    pool.step(PoolStep::SwapApproved { r#ref, leg: None })
        .unwrap();
    pool.step(PoolStep::SwapApproved { r#ref, leg: None })
        .unwrap();
    pool.step(PoolStep::SwapBegun {
        r#ref,
        before: None,
        leg: None,
        amount_in: None,
    })
    .unwrap();
    // begun, not undone: what happened at the pool is not known yet
    assert!(pool.step(fell_back()).is_err());
    pool.step(undone(20_000)).unwrap();
    assert!(pool.step(undone(20_000)).is_err(), "undone once");
    pool.step(fell_back()).unwrap();
    let payout = &pool.deposits[&r#ref].payout;
    assert_eq!(payout.swap.approvals, Some(2));
    assert_eq!(payout.swap.undone, Some(TokenAmount::from(20_000_u32)));
    assert_eq!(payout.swap.withdrawn, Some(usdc(99)));
    let attempt = payout.attempt.unwrap();
    assert_eq!(attempt.path, PayoutPath::Ckusdc);
    assert_eq!(attempt.amount, Some(usdc(99)));
    assert_eq!(attempt.token, Some(PrincipalBytes(ckusdc())));

    // a swap that paid its output: never undone, never approved again, its transfer fixed
    let paid_out = started(&mut pool, 2);
    pool.step(PoolStep::SwapBegun {
        r#ref: paid_out,
        before: None,
        leg: None,
        amount_in: None,
    })
    .unwrap();
    pool.step(PoolStep::Swapped {
        r#ref: paid_out,
        out: TokenAmount::from(9_000_u32),
        send: Some(TokenAmount::from(7_000_u32)),
        leg: None,
    })
    .unwrap();
    assert!(pool
        .step(PoolStep::SwapUndone {
            r#ref: paid_out,
            cost: TokenAmount::ZERO,
            withdrawn: TokenAmount::ZERO,
            leg: None,
        })
        .is_err());
    assert!(pool
        .step(PoolStep::SwapApproved {
            r#ref: paid_out,
            leg: None
        })
        .is_err());
    assert_eq!(
        pool.deposits[&paid_out].payout.swap.send,
        Some(TokenAmount::from(7_000_u32))
    );
}

#[test]
fn arrivals_above_a_read_block_are_not_expected_there() {
    let mut pool = funded(usdc(1_000));
    let r#ref = register(&mut pool, swap(1, usdc(100), usdc(1)));
    pool.step(PoolStep::Arrived {
        r#ref,
        credit: credit(1),
        amount: usdc(100),
        block: BlockNumber::new(50),
    })
    .unwrap();
    assert_eq!(
        pool.ledger
            .arrivals_above(ChainId::BASE, BlockNumber::new(49)),
        usdc(100)
    );
    assert_eq!(
        pool.ledger
            .arrivals_above(ChainId::BASE, BlockNumber::new(50)),
        TokenAmount::ZERO
    );
    let ledger = pool
        .ledger
        .clone()
        .seen_through(ChainId::BASE, BlockNumber::new(49));
    assert_eq!(ledger.unseen.len(), 1);
    let ledger = ledger.seen_through(ChainId::BASE, BlockNumber::new(50));
    assert!(ledger.unseen.is_empty());
}

#[test]
fn a_ckusdc_ledger_does_not_change_under_a_pool_that_holds_anything() {
    let mut pool = funded(usdc(1));
    let other = Principal::from_slice(&[3; 10]);
    let moved = PoolSettings {
        ckusdc_ledger: other,
        ledger_fees: vec![LedgerFee {
            ledger: other,
            fee: TokenAmount::from(FEE),
        }],
        ..settings()
    };
    assert_eq!(
        pool.step(PoolStep::SettingsSet {
            settings: moved.clone()
        }),
        Err(PoolStepError::CkusdcLedgerFixed)
    );
    let mut empty = Pool::default();
    empty
        .step(PoolStep::SettingsSet {
            settings: settings(),
        })
        .unwrap();
    empty
        .step(PoolStep::SettingsSet { settings: moved })
        .unwrap();
}

#[test]
fn a_step_round_trips_through_its_bytes() {
    let terms = swap(1, usdc(100), usdc(1));
    let steps = [
        PoolStep::SettingsSet {
            settings: settings(),
        },
        PoolStep::Registered {
            r#ref: terms.pool_ref().unwrap(),
            deposit: terms,
        },
        PoolStep::IcpChecked {
            check: crate::pool::IcpCheck {
                observed: usdc(5),
                expected: usdc(4),
                shortfall: false,
            },
        },
    ];
    for step in steps {
        assert_eq!(PoolStep::from_bytes(&step.to_bytes()), Some(step));
    }
}

/// Review fix: the caps are measured on what arrived, not on what was registered: a stake
/// that arrives past the TVL cap is no credit, and a swap has no cap on the canister
/// (2026-10-23).
/// Jeff's hold rule: a swap the ICP pocket cannot cover yet is credited all the same, its
/// payout owed and queued, first in first out; it starts (and holds) once the free ckUSDC
/// covers it, and a withdrawal waits behind it.
#[test]
fn an_arrival_is_measured_on_what_arrived() {
    let arrived = |r#ref, credit_log, amount| PoolStep::Arrived {
        r#ref,
        credit: credit(credit_log),
        amount,
        block: BlockNumber::new(10),
    };
    let started = |r#ref: PoolRef| PoolStep::PayoutStarted {
        r#ref,
        attempt: PayoutAttempt {
            created_at: Timestamp::from_nanos(5),
            memo: r#ref.into_bytes(),
            path: PayoutPath::Ckusdc,
            amount: None,
            token: None,
        },
    };
    let mut pool = funded(usdc(300));
    let first = register(&mut pool, swap(1, usdc(100), usdc(1)));
    let second = register(&mut pool, swap(2, usdc(100), usdc(1)));
    assert!(
        pool.ledger
            .arrival_fits(&pool.deposits[&first], usdc(1_001))
            .is_ok(),
        "a swap above the settings' per-swap cap fits"
    );
    // 400 arrives: a payout of 399 and its fee, more than the 300 free, credited and queued
    pool.step(arrived(first, 9, usdc(400))).unwrap();
    let held = usdc(399);
    assert_eq!(pool.deposits[&first].status, DepositStatus::Arrived);
    assert_eq!(pool.deposits[&first].held, Some(held));
    assert_eq!(pool.ledger.queue.head(), Some((first, held)));
    assert_eq!(pool.ledger.owed, held);
    assert_eq!(pool.ledger.pockets.icp.reserved, TokenAmount::ZERO);
    assert!(matches!(
        pool.step(started(first)),
        Err(PoolStepError::PocketShort { .. })
    ));
    // the second queues behind it, and may not start ahead of it
    pool.step(arrived(second, 10, usdc(100))).unwrap();
    assert_eq!(
        pool.step(started(second)),
        Err(PoolStepError::Terms("a payout starts first in first out"))
    );
    // a withdrawal waits behind the queue: nothing is free once the queue is counted
    assert_eq!(pool.ledger.icp_available(), TokenAmount::ZERO);
    let owner = recipient(200);
    let withdraw = PoolStep::WithdrawStarted {
        id: PoolOpId::new([0x42; 32]),
        owner,
        burned: pool.shares_of(&PoolAccount::Icp(owner).key()),
        usdc: usdc(1),
        to: owner,
        token_out: ckusdc(),
        min_out: 0,
        attempt: PayoutAttempt {
            created_at: Timestamp::from_nanos(6),
            memo: [0x42; 32],
            path: PayoutPath::Ckusdc,
            amount: None,
            token: None,
        },
        move_fee: TokenAmount::ZERO,
    };
    assert!(matches!(
        pool.step(withdraw),
        Err(PoolStepError::PocketShort { .. })
    ));
    // the pocket grows (a stake on ICP): the head starts and holds all it needs
    let id = PoolOpId::new([0x43; 32]);
    pool.step(PoolStep::StakeStarted {
        id,
        owner: PoolAccount::Icp(recipient(201)),
        from: recipient(201),
        amount: usdc(200),
        created_at: Timestamp::from_nanos(7),
        move_fee: TokenAmount::ZERO,
    })
    .unwrap();
    let shares = pool.ledger.mint_now(usdc(200)).unwrap();
    pool.step(PoolStep::StakedIcp {
        id,
        owner: PoolAccount::Icp(recipient(201)),
        amount: usdc(200),
        move_fee: TokenAmount::ZERO,
        shares,
        block: 3,
    })
    .unwrap();
    pool.step(started(first)).unwrap();
    assert_eq!(pool.ledger.pockets.icp.reserved, held);
    assert_eq!(
        pool.ledger.pockets.icp.free,
        usdc(500).checked_sub(held).unwrap()
    );
    assert_eq!(pool.ledger.queue.head().map(|(head, _)| head), Some(second));
    pool.step(PoolStep::Paid {
        r#ref: first,
        token: ckusdc(),
        amount: usdc(399),
        block: 1,
        spent: held,
    })
    .unwrap();
    assert_eq!(pool.ledger.pockets.icp.reserved, TokenAmount::ZERO);

    // a stake registered small and arriving large: past the TVL cap is no credit
    let mut full = funded(usdc(9_990));
    let stake_ref = register(&mut full, stake(2, usdc(10), TokenAmount::ZERO));
    assert_eq!(
        full.step(arrived(stake_ref, 9, usdc(5_000))),
        Err(PoolStepError::DoesNotFit("above the TVL cap"))
    );
    full.step(arrived(stake_ref, 9, usdc(10))).unwrap();
}

/// P11d (2026-10-21): no premium floor, the premium exactly the settings' share rounded up;
/// the ICP fees' floor is the path's own ckUSDC fees: 1 for ckUSDC, 2 for a direct token,
/// 3 through ICP. A token's own fee is never added to micro USDC.
#[test]
fn the_icp_fees_floor_is_the_paths_own_ckusdc_fees() {
    let token = Principal::from_slice(&[0x70; 10]);
    let settings = PoolSettings {
        icp_tokens: vec![crate::pool::IcpToken {
            ledger: token,
            icpswap_pool: Principal::from_slice(&[0x71; 10]),
        }],
        ledger_fees: vec![
            LedgerFee {
                ledger: ckusdc(),
                fee: TokenAmount::from(FEE),
            },
            // a token whose fee in its own units is far larger than a micro USDC amount
            LedgerFee {
                ledger: token,
                fee: TokenAmount::from(70_000_000_u32),
            },
        ],
        ..settings()
    };
    // the ckUSDC fees decide, never the token's
    assert_eq!(
        icp_fees_floor(&settings, ckusdc()),
        Some(TokenAmount::from(FEE))
    );
    assert_eq!(
        icp_fees_floor(&settings, token),
        Some(TokenAmount::from(2 * FEE))
    );
    // a small swap's premium is its share alone
    assert_eq!(
        premium_share(&settings, usdc(2)),
        Some(TokenAmount::from(2_000_u32))
    );
    // through ICP: two legs, eight fees
    let via = Principal::from_slice(&[0x72; 10]);
    let icp = Principal::from_slice(&[0x73; 10]);
    let icp_pool = Principal::from_slice(&[0x74; 10]);
    let routed = PoolSettings {
        routes: Some(crate::pool::IcpRoutes {
            icp_ckusdc_pool: Some(PrincipalBytes(icp_pool)),
            icp_ledger: Some(PrincipalBytes(icp)),
            routes: vec![
                crate::pool::IcpRoute {
                    ledger: token,
                    via: crate::pool::IcpRouteVia::Direct(Principal::from_slice(&[0x71; 10])),
                },
                crate::pool::IcpRoute {
                    ledger: via,
                    via: crate::pool::IcpRouteVia::Icp(Principal::from_slice(&[0x75; 10])),
                },
            ],
            ledger_fees: Vec::new(),
            pool_tokens: Vec::new(),
        }),
        ..settings.clone()
    };
    assert_eq!(
        icp_fees_floor(&routed, via),
        Some(TokenAmount::from(3 * FEE))
    );
    assert_eq!(
        icp_fees_floor(&routed, token),
        Some(TokenAmount::from(2 * FEE))
    );
    // once routes are set they alone list: a token only in icp_tokens is not
    let unrouted = Principal::from_slice(&[0x76; 10]);
    let mut listed_before = routed.clone();
    listed_before.icp_tokens.push(crate::pool::IcpToken {
        ledger: unrouted,
        icpswap_pool: Principal::from_slice(&[0x77; 10]),
    });
    assert!(!listed_before.is_listed(unrouted));
    // a large one: 10 bps of it, rounded up
    let large = usdc(1_000).checked_add(TokenAmount::ONE).unwrap();
    assert_eq!(
        premium_share(&settings, large),
        Some(TokenAmount::from(1_000_001_u32))
    );
}

/// Review fix (head of line): the queue stays first in first out, its head's need of the ICP
/// pocket is read off the ledger (what P11c's urgent rebalance moves), with when it became
/// the head. A head waiting for the pocket is never failed (its drive answers that it is
/// queued); one whose drive fails before it starts is counted (CR-08), and stays the head
/// until the usual attempts.
#[test]
fn the_queues_head_has_its_need_and_its_wait() {
    let mut pool = funded(usdc(100));
    let first = register(&mut pool, swap(1, usdc(300), usdc(1)));
    let second = register(&mut pool, swap(2, usdc(50), usdc(1)));
    assert_eq!(pool.ledger.queue_need(), None);
    pool.step(PoolStep::Arrived {
        r#ref: first,
        credit: credit(1),
        amount: usdc(300),
        block: BlockNumber::new(10),
    })
    .unwrap();
    // 299 held once it starts (its fees the user's, out of it), 100 free
    let held = usdc(299);
    assert_eq!(
        pool.ledger.queue_need(),
        Some(held.checked_sub(usdc(100)).unwrap())
    );
    assert_eq!(pool.ledger.head_since, Some(Timestamp::from_nanos(1)));
    pool.step(PoolStep::Arrived {
        r#ref: second,
        credit: credit(2),
        amount: usdc(50),
        block: BlockNumber::new(10),
    })
    .unwrap();
    // the head's wait does not move for what queues behind it
    assert_eq!(pool.ledger.head_since, Some(Timestamp::from_nanos(1)));
    pool.step(PoolStep::AttemptFailed {
        r#ref: first,
        why: "no ckUSDC fee was read".into(),
    })
    .unwrap();
    assert_eq!(pool.deposits[&first].payout.failures, 1);
    assert_eq!(pool.ledger.queue.head().map(|(head, _)| head), Some(first));
    assert_eq!(pool.ledger.head_since, Some(Timestamp::from_nanos(1)));
}

/// A path through ICP (P11b): its second leg only once the first paid out, its way back only
/// once the second cannot pay, and its fall back to ckUSDC only once the way back brought the
/// ICP back; never once the second leg paid out.
#[test]
fn a_path_through_icp_runs_its_legs_in_order() {
    use crate::pool::{LEG_BACK, LEG_SECOND};
    let mut pool = funded(usdc(1_000));
    let r#ref = register(&mut pool, swap(2, usdc(100), usdc(1)));
    pool.step(PoolStep::Arrived {
        r#ref,
        credit: credit(2),
        amount: usdc(100),
        block: BlockNumber::new(10),
    })
    .unwrap();
    pool.step(PoolStep::PayoutStarted {
        r#ref,
        attempt: PayoutAttempt {
            created_at: Timestamp::from_nanos(5),
            memo: r#ref.into_bytes(),
            path: PayoutPath::IcpswapVia {
                first: Principal::from_slice(&[5; 10]),
                second: Principal::from_slice(&[6; 10]),
                icp: Principal::from_slice(&[7; 10]),
            },
            amount: Some(usdc(99)),
            token: None,
        },
    })
    .unwrap();
    let begun = |leg| PoolStep::SwapBegun {
        r#ref,
        before: None,
        leg,
        amount_in: Some(TokenAmount::from(1_000_u32)),
    };
    let swapped = |leg| PoolStep::Swapped {
        r#ref,
        out: TokenAmount::from(900_u32),
        send: None,
        leg,
    };
    let fell_back = PoolStep::FellBack {
        r#ref,
        why: "the second leg could not pay".into(),
        amount: Some(usdc(99)),
        token: None,
    };
    // the second leg and the way back wait for the first's output
    assert!(pool.step(begun(Some(LEG_SECOND))).is_err());
    assert!(pool.step(begun(Some(LEG_BACK))).is_err());
    pool.step(begun(None)).unwrap();
    pool.step(swapped(None)).unwrap();
    assert_eq!(
        pool.deposits[&r#ref].payout.swap.amount_in,
        Some(TokenAmount::from(1_000_u32))
    );
    // the first leg paid out: no fall back before the way back brought the ICP back
    assert!(pool.step(fell_back.clone()).is_err());
    pool.step(PoolStep::SwapDeclined {
        r#ref,
        why: "short".into(),
        leg: Some(LEG_SECOND),
    })
    .unwrap();
    // the way back, once the second leg did not begin; then the second may not begin
    pool.step(begun(Some(LEG_BACK))).unwrap();
    pool.step(swapped(Some(LEG_BACK))).unwrap();
    pool.step(fell_back).unwrap();
    let payout = &pool.deposits[&r#ref].payout;
    assert_eq!(payout.back.unwrap().out, Some(TokenAmount::from(900_u32)));
    assert!(payout.fell_back.is_some());
}

/// P11d (2026-10-21): a deposit's transfer the canister's own read found without its terms
/// awaits them, on the unattributed line; its terms claim it and its arrival credits it once,
/// on the Base balance's books once; terms the pool does not take hold it for a controller.
#[test]
fn a_transfer_awaiting_its_terms_is_claimed_once() {
    use crate::pool::OrphanStatus;
    let mut pool = funded(usdc(1_000));
    let terms = swap(1, usdc(100), usdc(1));
    let r#ref = terms.pool_ref().unwrap();
    let until = Timestamp::from_nanos(500);
    let awaiting = |log| PoolStep::Unattributed {
        credit: credit(log),
        amount: usdc(100),
        src_payer: Some(payer()),
        why: "awaiting its terms".into(),
        block: BlockNumber::new(10),
        r#ref: Some(r#ref),
        awaiting_until: Some(until),
    };
    pool.step(awaiting(1)).unwrap();
    assert_eq!(pool.ledger.unattributed, usdc(100));
    assert_eq!(
        pool.orphans[&credit(1)].status,
        OrphanStatus::AwaitingTerms { r#ref, until }
    );
    let unseen = pool.ledger.unseen.len();
    let arrived = PoolStep::Arrived {
        r#ref,
        credit: credit(1),
        amount: usdc(100),
        block: BlockNumber::new(10),
    };
    pool.step(PoolStep::Registered {
        r#ref,
        deposit: terms,
    })
    .unwrap();
    // credited to nothing until claimed
    assert_eq!(
        pool.step(arrived.clone()),
        Err(PoolStepError::AlreadyCredited(credit(1)))
    );
    pool.step(PoolStep::TermsClaimed {
        credit: credit(1),
        refused: None,
    })
    .unwrap();
    assert_eq!(pool.ledger.unattributed, TokenAmount::ZERO);
    pool.step(arrived).unwrap();
    assert_eq!(pool.orphans[&credit(1)].status, OrphanStatus::Credited);
    assert_eq!(pool.ledger.unseen.len(), unseen, "on the books once");
    assert_eq!(pool.ledger.owed, usdc(99));
    assert_eq!(pool.deposits[&r#ref].status, DepositStatus::Arrived);
    // claimed once
    assert!(pool
        .step(PoolStep::TermsClaimed {
            credit: credit(1),
            refused: None,
        })
        .is_err());
    // terms the pool does not take: held for a controller with why, still unattributed
    pool.step(awaiting(2)).unwrap();
    pool.step(PoolStep::TermsClaimed {
        credit: credit(2),
        refused: Some("the pool is closed".into()),
    })
    .unwrap();
    assert_eq!(pool.orphans[&credit(2)].status, OrphanStatus::Held);
    assert_eq!(pool.orphans[&credit(2)].why, "the pool is closed");
    assert_eq!(pool.ledger.unattributed, usdc(100));
}

/// CR-08: a queued payout whose drive fails before it starts (its amount cannot cover its
/// path's fees, a fee never read) is counted like a started one and, at the usual attempts,
/// held for a human: it leaves the queue, so the payouts behind it go on. Nothing it holds
/// moves: still owed, never reserved.
#[test]
fn a_queued_payout_that_never_starts_is_held_for_a_human_and_frees_the_queue() {
    let arrived = |r#ref, credit_log, amount| PoolStep::Arrived {
        r#ref,
        credit: credit(credit_log),
        amount,
        block: BlockNumber::new(10),
    };
    let mut pool = funded(usdc(300));
    let first = register(&mut pool, swap(1, usdc(100), usdc(1)));
    let second = register(&mut pool, swap(2, usdc(100), usdc(1)));
    pool.step(arrived(first, 9, usdc(400))).unwrap();
    pool.step(arrived(second, 10, usdc(100))).unwrap();
    assert_eq!(pool.ledger.queue.head().map(|(head, _)| head), Some(first));
    let (owed, icp) = (pool.ledger.owed, pool.ledger.pockets.icp);
    let failed = || PoolStep::AttemptFailed {
        r#ref: first,
        why: "the amount does not cover the fees".to_string(),
    };
    for _ in 1..MAX_PAYOUT_ATTEMPTS {
        pool.step(failed()).unwrap();
        assert_eq!(pool.ledger.queue.head().map(|(head, _)| head), Some(first));
    }
    pool.step(failed()).unwrap();
    assert!(matches!(
        pool.deposits[&first].status,
        DepositStatus::PayoutFailed(_)
    ));
    assert_eq!(
        pool.ledger.queue.head().map(|(head, _)| head),
        Some(second),
        "the payout behind it is the head now"
    );
    assert_eq!(pool.ledger.owed, owed, "still owed to its user");
    assert_eq!(
        pool.ledger.pockets.icp, icp,
        "nothing reserved, nothing moved"
    );
    assert!(
        pool.step(failed()).is_err(),
        "a payout held for a human is done"
    );
}

/// A swap of 100 USDC (premium 1) arrived for `byte` and started on the ckUSDC path.
fn started(pool: &mut Pool, byte: u8, log: u64) -> PoolRef {
    let r#ref = register(pool, swap(byte, usdc(100), usdc(1)));
    pool.step(PoolStep::Arrived {
        r#ref,
        credit: credit(log),
        amount: usdc(100),
        block: BlockNumber::new(10),
    })
    .unwrap();
    pool.step(PoolStep::PayoutStarted {
        r#ref,
        attempt: PayoutAttempt {
            created_at: Timestamp::from_nanos(5),
            memo: r#ref.into_bytes(),
            path: PayoutPath::Ckusdc,
            amount: None,
            token: None,
        },
    })
    .unwrap();
    r#ref
}

/// [`started`], then held for a human after its tenth failure, no transfer asked.
fn held_started(pool: &mut Pool, byte: u8, log: u64) -> PoolRef {
    let r#ref = started(pool, byte, log);
    hold(pool, r#ref);
    r#ref
}

/// The ckUSDC transfer a started payout of 99 USDC asks: 99 less one fee, its fee spent.
fn asked(r#ref: PoolRef) -> PoolStep {
    PoolStep::TransferAsked {
        r#ref,
        token: PrincipalBytes(ckusdc()),
        amount: usdc(99).checked_sub(TokenAmount::from(FEE)).unwrap(),
        spent: usdc(99),
    }
}

/// The review (2026-10-24): a payout's transfer to its recipient is recorded as asked before
/// its call (it may have landed from then on) and as refused once the ledger refuses it for
/// sure; asked again, it may have landed again. A transfer asked on other terms while one may
/// have landed is refused.
#[test]
fn a_transfer_asked_may_have_landed_until_the_ledger_refuses_it() {
    let mut pool = funded(usdc(1_000));
    let r#ref = started(&mut pool, 1, 1);
    let sent = |pool: &Pool| pool.deposits[&r#ref].payout.sent;
    assert_eq!(sent(&pool), None);
    assert!(pool.step(PoolStep::TransferRefused { r#ref }).is_err());
    pool.step(asked(r#ref)).unwrap();
    assert_eq!(
        sent(&pool),
        Some(Sent {
            token: PrincipalBytes(ckusdc()),
            amount: usdc(99).checked_sub(TokenAmount::from(FEE)).unwrap(),
            spent: usdc(99),
            refused: false,
            lost: crate::pool::payout::Flag(false),
        })
    );
    let mut other = asked(r#ref);
    if let PoolStep::TransferAsked { amount, .. } = &mut other {
        *amount = usdc(1);
    }
    assert!(pool.step(other.clone()).is_err());
    pool.step(PoolStep::TransferRefused { r#ref }).unwrap();
    assert!(sent(&pool).unwrap().refused);
    assert!(pool.step(PoolStep::TransferRefused { r#ref }).is_err());
    // refused for sure, it is asked again, on other terms too
    pool.step(asked(r#ref)).unwrap();
    assert!(!sent(&pool).unwrap().refused);
}

/// Fails `r#ref`'s payout until it is held for a human.
fn hold(pool: &mut Pool, r#ref: PoolRef) {
    for n in 1..=MAX_PAYOUT_ATTEMPTS {
        pool.step(PoolStep::AttemptFailed {
            r#ref,
            why: format!("try {n}"),
        })
        .unwrap();
    }
    assert!(matches!(
        pool.deposits[&r#ref].status,
        DepositStatus::PayoutFailed(_)
    ));
}

fn resolved(r#ref: PoolRef, resolution: DepositResolution, retry_at: Option<u64>) -> PoolStep {
    PoolStep::DepositResolved {
        r#ref,
        resolution,
        retry_at: retry_at.map(Timestamp::from_nanos),
    }
}

/// A swap of 100 USDC (premium 1) arrived for `byte` and queued, held for a human after its
/// tenth failure before it started (CR-08).
fn held_never_started(pool: &mut Pool, byte: u8, log: u64) -> PoolRef {
    let r#ref = register(pool, swap(byte, usdc(100), usdc(1)));
    pool.step(PoolStep::Arrived {
        r#ref,
        credit: credit(log),
        amount: usdc(100),
        block: BlockNumber::new(10),
    })
    .unwrap();
    hold(pool, r#ref);
    r#ref
}

/// Jeff (2026-10-24), `resolve_deposit`'s `Retry`: a deposit held for a human goes back to
/// its payout with fresh attempts. A started one whose transfer may have landed is paying
/// again with its own dedup pair (so a transfer that landed is answered `Duplicate`); one
/// whose transfer cannot land twice (never asked, or refused for sure) with the instant the
/// door gave it; either still held. One that never started (CR-08) is queued again, at the
/// back. Once.
#[test]
fn a_held_deposit_retried_goes_back_to_its_payout_with_fresh_attempts() {
    let mut pool = funded(usdc(1_000));
    let landed = started(&mut pool, 9, 9);
    pool.step(asked(landed)).unwrap();
    hold(&mut pool, landed);
    assert!(
        pool.step(resolved(landed, DepositResolution::Retry, Some(7)))
            .is_err(),
        "its own pair, while it may have landed"
    );
    pool.step(resolved(landed, DepositResolution::Retry, None))
        .unwrap();
    assert_eq!(
        pool.deposits[&landed].payout.attempt.unwrap().created_at,
        Timestamp::from_nanos(5)
    );
    let started = held_started(&mut pool, 1, 1);
    let (owed, icp) = (pool.ledger.owed, pool.ledger.pockets.icp);
    // nothing can land twice: asked afresh, at the door's instant
    pool.step(resolved(started, DepositResolution::Retry, Some(7)))
        .unwrap();
    let deposit = &pool.deposits[&started];
    assert_eq!(deposit.status, DepositStatus::Paying);
    assert_eq!(deposit.payout.failures, 0);
    assert_eq!(deposit.payout.failed, None);
    assert_eq!(
        deposit.payout.attempt.unwrap().created_at,
        Timestamp::from_nanos(7)
    );
    assert_eq!((pool.ledger.owed, pool.ledger.pockets.icp), (owed, icp));
    assert!(pool.ledger.paying.contains(&started));
    // resolved once; its payout fails again as before
    assert!(pool
        .step(resolved(started, DepositResolution::Retry, None))
        .is_err());
    pool.step(PoolStep::AttemptFailed {
        r#ref: started,
        why: "again".into(),
    })
    .unwrap();

    // CR-08: never started, held; another deposit queued behind it meanwhile
    let never = held_never_started(&mut pool, 2, 2);
    let behind = register(&mut pool, swap(3, usdc(100), usdc(1)));
    pool.step(PoolStep::Arrived {
        r#ref: behind,
        credit: credit(3),
        amount: usdc(100),
        block: BlockNumber::new(10),
    })
    .unwrap();
    let icp = pool.ledger.pockets.icp;
    pool.step(resolved(never, DepositResolution::Retry, None))
        .unwrap();
    assert_eq!(pool.deposits[&never].status, DepositStatus::Arrived);
    assert_eq!(pool.deposits[&never].payout.failures, 0);
    let queued: Vec<PoolRef> = pool.ledger.queue.0.iter().map(|(r, _)| *r).collect();
    assert_eq!(queued, vec![behind, never], "at the back");
    assert_eq!(pool.ledger.pockets.icp, icp, "nothing reserved");
}

/// `resolve_deposit`'s `Refund`: what arrived goes back to its payer on Base through the
/// orphan refund path. The deposit is done (`Refunded`), owed nothing and out of `paying`;
/// its arrival leaves the Base pocket's free line for the unattributed line, an orphan held
/// under its transfer's key, so the Base balance's books do not move. Stakers give back
/// only the premium they had booked.
#[test]
fn a_held_deposit_refunded_is_an_orphan_held_for_its_payer() {
    let mut pool = funded(usdc(1_000));
    let never = held_never_started(&mut pool, 1, 1);
    let (base_held, assets, owed, icp) = (
        pool.ledger.base_held(),
        pool.ledger.total_assets(),
        pool.ledger.owed,
        pool.ledger.pockets.icp,
    );
    let held = pool.deposits[&never].held();
    // a refund carries no dedup instant
    assert!(pool
        .step(resolved(never, DepositResolution::Refund, Some(7)))
        .is_err());
    pool.step(resolved(never, DepositResolution::Refund, None))
        .unwrap();
    let deposit = &pool.deposits[&never];
    assert_eq!(deposit.status, DepositStatus::Refunded);
    assert!(!pool.ledger.paying.contains(&never));
    assert_eq!(pool.ledger.owed, owed.checked_sub(held).unwrap());
    assert_eq!(
        pool.ledger.pockets.icp, icp,
        "it never held anything on ICP"
    );
    assert_eq!(pool.ledger.base_held(), base_held);
    assert_eq!(pool.ledger.unattributed, usdc(100));
    assert_eq!(
        pool.ledger.total_assets(),
        assets.checked_sub(usdc(1)).unwrap()
    );
    let orphan = &pool.orphans[&credit(1)];
    assert_eq!(orphan.amount, usdc(100));
    assert_eq!(orphan.payer, Some(payer()));
    assert_eq!(orphan.status, crate::pool::OrphanStatus::Held);
    assert!(pool
        .step(resolved(never, DepositResolution::Refund, None))
        .is_err());

    // a stake held for a human: all of it back, the stakers' assets unchanged
    let staked = register(&mut pool, stake(4, usdc(50), TokenAmount::ZERO));
    pool.step(PoolStep::Arrived {
        r#ref: staked,
        credit: credit(4),
        amount: usdc(50),
        block: BlockNumber::new(11),
    })
    .unwrap();
    hold(&mut pool, staked);
    let assets = pool.ledger.total_assets();
    pool.step(resolved(staked, DepositResolution::Refund, None))
        .unwrap();
    assert_eq!(pool.ledger.total_assets(), assets);
    assert_eq!(pool.orphans[&credit(4)].amount, usdc(50));
}

/// The review (2026-10-24): a refund is allowed only when the canister knows nothing went
/// out. A started payout that asked no transfer and moved nothing on ICP, or whose transfer
/// the ledger refused for sure at its last ask, is refunded, its hold free again on ICP; one
/// whose transfer may have landed, or whose ICPSwap leg moved money, is not. Nor is a deposit
/// whose payer is the zero address, or one the Base pocket's free USDC no longer covers.
#[test]
fn a_held_deposit_is_refunded_only_when_nothing_went_out() {
    let mut pool = funded(usdc(1_000));
    let unasked = held_started(&mut pool, 1, 1);
    let icp = pool.ledger.pockets.icp;
    let held = pool.deposits[&unasked].held();
    pool.step(resolved(unasked, DepositResolution::Refund, None))
        .unwrap();
    assert_eq!(
        pool.ledger.pockets.icp.free,
        icp.free.checked_add(held).unwrap()
    );
    assert_eq!(
        pool.ledger.pockets.icp.reserved,
        icp.reserved.checked_sub(held).unwrap()
    );
    let landed = started(&mut pool, 2, 2);
    pool.step(asked(landed)).unwrap();
    hold(&mut pool, landed);
    assert!(pool
        .step(resolved(landed, DepositResolution::Refund, None))
        .is_err());
    let refused = started(&mut pool, 3, 3);
    pool.step(asked(refused)).unwrap();
    pool.step(PoolStep::TransferRefused { r#ref: refused })
        .unwrap();
    hold(&mut pool, refused);
    pool.step(resolved(refused, DepositResolution::Refund, None))
        .unwrap();
    assert_eq!(pool.deposits[&refused].status, DepositStatus::Refunded);
    let approved = held_started(&mut pool, 4, 4);
    let mut deposit = pool.deposits[&approved].clone();
    deposit.payout.swap.approvals = Some(1);
    pool.deposits.insert(approved, deposit);
    assert!(pool
        .step(resolved(approved, DepositResolution::Refund, None))
        .is_err());
    let nobody = held_never_started(&mut pool, 5, 5);
    let mut deposit = pool.deposits[&nobody].clone();
    deposit.terms.src_payer = EvmAddress::ZERO;
    pool.deposits.insert(nobody, deposit);
    assert!(pool
        .step(resolved(nobody, DepositResolution::Refund, None))
        .is_err());
    let short = held_never_started(&mut pool, 6, 6);
    pool.ledger.pockets.base.free = usdc(99);
    assert!(pool
        .step(resolved(short, DepositResolution::Refund, None))
        .is_err());
    // each the canister cannot refund may still be retried
    for r#ref in [landed, approved, nobody, short] {
        pool.step(resolved(r#ref, DepositResolution::Retry, None))
            .unwrap();
    }
}

/// The review (2026-10-24): a started payout whose transfer may have landed is retried with
/// its old dedup pair only when that transfer is on the ckUSDC ledger it started with, which
/// deduplicates it; one that may have landed on another token's ledger is never retried
/// (that ledger may take the old pair as a new transfer), with any instant.
#[test]
fn a_started_payout_that_may_have_paid_another_token_is_never_retried() {
    let mut pool = funded(usdc(1_000));
    let token = Principal::from_slice(&[0x70; 10]);
    let mut terms = swap(1, usdc(100), usdc(1));
    terms.kind = DepositKind::Swap {
        recipient: recipient(1),
        token_out: token,
        min_out: 1,
    };
    let r#ref = register(&mut pool, terms);
    pool.step(PoolStep::Arrived {
        r#ref,
        credit: credit(1),
        amount: usdc(100),
        block: BlockNumber::new(10),
    })
    .unwrap();
    pool.step(PoolStep::PayoutStarted {
        r#ref,
        attempt: PayoutAttempt {
            created_at: Timestamp::from_nanos(5),
            memo: r#ref.into_bytes(),
            path: PayoutPath::Icpswap {
                pool: Principal::from_slice(&[0x71; 10]),
            },
            amount: None,
            token: None,
        },
    })
    .unwrap();
    // its transfer on the token's ledger asked, and its answer lost: it may have landed
    pool.step(PoolStep::TransferAsked {
        r#ref,
        token: PrincipalBytes(token),
        amount: usdc(5),
        spent: usdc(99),
    })
    .unwrap();
    hold(&mut pool, r#ref);
    assert_eq!(pool.deposits[&r#ref].out_ledger(ckusdc()), Some(token));
    for retry_at in [None, Some(7)] {
        assert!(pool
            .step(resolved(r#ref, DepositResolution::Retry, retry_at))
            .is_err());
    }
}

/// The review (2026-10-24): a started payout whose transfer cannot land twice (never asked,
/// or refused for sure) is retried whatever token it pays, asked afresh; a payout that never
/// started has no instant to give.
#[test]
fn a_started_payout_whose_transfer_cannot_land_twice_is_retried_whatever_it_pays() {
    let mut pool = funded(usdc(1_000));
    let token = Principal::from_slice(&[0x70; 10]);
    let mut terms = swap(1, usdc(100), usdc(1));
    terms.kind = DepositKind::Swap {
        recipient: recipient(1),
        token_out: token,
        min_out: 1,
    };
    let r#ref = register(&mut pool, terms);
    pool.step(PoolStep::Arrived {
        r#ref,
        credit: credit(1),
        amount: usdc(100),
        block: BlockNumber::new(10),
    })
    .unwrap();
    pool.step(PoolStep::PayoutStarted {
        r#ref,
        attempt: PayoutAttempt {
            created_at: Timestamp::from_nanos(5),
            memo: r#ref.into_bytes(),
            path: PayoutPath::Icpswap {
                pool: Principal::from_slice(&[0x71; 10]),
            },
            amount: None,
            token: None,
        },
    })
    .unwrap();
    pool.step(PoolStep::TransferAsked {
        r#ref,
        token: PrincipalBytes(token),
        amount: usdc(5),
        spent: usdc(99),
    })
    .unwrap();
    pool.step(PoolStep::TransferRefused { r#ref }).unwrap();
    hold(&mut pool, r#ref);
    pool.step(resolved(r#ref, DepositResolution::Retry, Some(8)))
        .unwrap();
    assert_eq!(
        pool.deposits[&r#ref].payout.attempt.unwrap().created_at,
        Timestamp::from_nanos(8)
    );
    let never = held_never_started(&mut pool, 2, 2);
    assert!(pool
        .step(resolved(never, DepositResolution::Retry, Some(8)))
        .is_err());
}

/// The review (2026-10-24): `MarkPaid` books a started payout's transfer, as the payout
/// asked it, paid in the block the door read, once: owed nothing, its hold spent.
#[test]
fn a_held_deposit_marked_paid_is_paid_as_its_transfer_was_asked() {
    let mut pool = funded(usdc(1_000));
    let mark = |r#ref, block| resolved(r#ref, DepositResolution::MarkPaid { block }, None);
    let unasked = held_started(&mut pool, 1, 1);
    assert!(pool.step(mark(unasked, 77)).is_err(), "no transfer asked");
    let never = held_never_started(&mut pool, 2, 2);
    assert!(pool.step(mark(never, 77)).is_err(), "never started");
    let landed = started(&mut pool, 3, 3);
    pool.step(asked(landed)).unwrap();
    hold(&mut pool, landed);
    let (owed, icp) = (pool.ledger.owed, pool.ledger.pockets.icp);
    let held = pool.deposits[&landed].held();
    pool.step(mark(landed, 77)).unwrap();
    let deposit = &pool.deposits[&landed];
    assert_eq!(deposit.status, DepositStatus::Paid);
    assert_eq!(
        deposit.paid,
        Some(crate::pool::Paid {
            token: ckusdc(),
            amount: usdc(99).checked_sub(TokenAmount::from(FEE)).unwrap(),
            block: 77,
            spent: usdc(99),
        })
    );
    assert!(!pool.ledger.paying.contains(&landed));
    assert_eq!(pool.ledger.owed, owed.checked_sub(held).unwrap());
    assert_eq!(
        pool.ledger.pockets.icp.reserved,
        icp.reserved.checked_sub(held).unwrap()
    );
    assert!(pool.step(mark(landed, 77)).is_err(), "once");
}

/// `resolve_stake` (Jeff, 2026-10-24): an ICP stake whose pull's answer was lost past the
/// ledger's deduplication window. `Pulled` (a controller saw the ledger block) mints its
/// shares and books its ckUSDC free on ICP; `NotPulled` drops it and frees its room. Once.
#[test]
fn a_stake_whose_pull_was_lost_is_resolved_once() {
    let mut pool = funded(usdc(1_000));
    let start = |pool: &mut Pool, byte: u8| {
        let id = PoolOpId::new([byte; 32]);
        pool.step(PoolStep::StakeStarted {
            id,
            owner: PoolAccount::Icp(recipient(byte)),
            from: recipient(byte),
            amount: usdc(10),
            created_at: Timestamp::from_nanos(2),
            move_fee: TokenAmount::ZERO,
        })
        .unwrap();
        id
    };
    let pulled = start(&mut pool, 7);
    let (room, icp, total) = (
        pool.ledger.stake_room,
        pool.ledger.pockets.icp,
        pool.ledger.total_shares,
    );
    let shares = pool.ledger.mint_now(usdc(10)).unwrap();
    assert!(pool
        .step(PoolStep::StakeResolved {
            id: pulled,
            resolution: StakeResolution::Pulled {
                block: 9,
                shares: Shares::ZERO
            },
        })
        .is_err());
    let step = PoolStep::StakeResolved {
        id: pulled,
        resolution: StakeResolution::Pulled { block: 9, shares },
    };
    pool.step(step.clone()).unwrap();
    assert!(!pool.ledger.pending.contains_key(&pulled));
    assert_eq!(pool.ledger.stake_room, room.checked_sub(usdc(10)).unwrap());
    assert_eq!(
        pool.ledger.pockets.icp.free,
        icp.free.checked_add(usdc(10)).unwrap()
    );
    assert_eq!(pool.ledger.total_shares, total.checked_add(shares).unwrap());
    assert_eq!(
        pool.shares_of(&PoolAccount::Icp(recipient(7)).key()),
        shares
    );
    assert!(pool.step(step).is_err(), "once");

    let dropped = start(&mut pool, 8);
    let (room, icp, total) = (
        pool.ledger.stake_room,
        pool.ledger.pockets.icp,
        pool.ledger.total_shares,
    );
    pool.step(PoolStep::StakeResolved {
        id: dropped,
        resolution: StakeResolution::NotPulled,
    })
    .unwrap();
    assert!(!pool.ledger.pending.contains_key(&dropped));
    assert_eq!(pool.ledger.stake_room, room.checked_sub(usdc(10)).unwrap());
    assert_eq!(pool.ledger.pockets.icp, icp);
    assert_eq!(pool.ledger.total_shares, total);
}

/// A stake held for a human after its mint kept failing, retried: back to `Arrived` to mint
/// again, never queued (a stake takes nothing of the ICP pocket), still owed until it mints.
#[test]
fn a_held_stake_retried_mints_again() {
    let mut pool = funded(usdc(1_000));
    let staked = register(&mut pool, stake(4, usdc(50), TokenAmount::ZERO));
    pool.step(PoolStep::Arrived {
        r#ref: staked,
        credit: credit(4),
        amount: usdc(50),
        block: BlockNumber::new(11),
    })
    .unwrap();
    hold(&mut pool, staked);
    let owed = pool.ledger.owed;
    pool.step(resolved(staked, DepositResolution::Retry, None))
        .unwrap();
    assert_eq!(pool.deposits[&staked].status, DepositStatus::Arrived);
    assert_eq!(pool.deposits[&staked].payout.failures, 0);
    assert!(pool.ledger.queue.0.is_empty());
    assert_eq!(pool.ledger.owed, owed);
    let shares = pool.ledger.mint_now(usdc(50)).unwrap();
    pool.step(PoolStep::Minted {
        r#ref: staked,
        owner: PoolAccount::Icp(recipient(4)),
        shares,
    })
    .unwrap();
    assert_eq!(pool.deposits[&staked].status, DepositStatus::Paid);
}

/// The review (2026-10-24): one ckUSDC ledger block resolves one lost stake, so a block a
/// controller named once is refused for any other.
#[test]
fn a_ledger_block_resolves_one_lost_stake() {
    let mut pool = funded(usdc(1_000));
    let start = |pool: &mut Pool, byte: u8| {
        let id = PoolOpId::new([byte; 32]);
        pool.step(PoolStep::StakeStarted {
            id,
            owner: PoolAccount::Icp(recipient(byte)),
            from: recipient(byte),
            amount: usdc(10),
            created_at: Timestamp::from_nanos(2),
            move_fee: TokenAmount::ZERO,
        })
        .unwrap();
        id
    };
    let (first, second) = (start(&mut pool, 7), start(&mut pool, 8));
    let pulled = |id, block, pool: &Pool| PoolStep::StakeResolved {
        id,
        resolution: StakeResolution::Pulled {
            block,
            shares: pool.ledger.mint_now(usdc(10)).unwrap(),
        },
    };
    pool.step(pulled(first, 9, &pool)).unwrap();
    assert!(pool.step(pulled(second, 9, &pool)).is_err());
    assert!(pool.ledger.pending.contains_key(&second));
    pool.step(pulled(second, 10, &pool)).unwrap();
    assert_eq!(
        pool.ledger.resolved_pulls,
        Some([9_u64, 10].into_iter().collect())
    );
}

/// The review (2026-10-24): an answer of a payout's transfer that was lost (unknown, or too
/// old to tell) is kept for good: a later refusal does not prove the lost ask landed nowhere
/// (a ledger that does not deduplicate may refuse a re-ask because the first drained the
/// money). Such a payout is neither refunded nor asked afresh; only its own dedup pair is
/// asked again, on the ckUSDC ledger it started with.
#[test]
fn a_transfer_whose_answer_was_lost_may_have_landed_even_once_refused() {
    let mut pool = funded(usdc(1_000));
    let r#ref = started(&mut pool, 1, 1);
    assert!(pool
        .step(PoolStep::TransferLost { r#ref, held: None })
        .is_err());
    pool.step(asked(r#ref)).unwrap();
    pool.step(PoolStep::TransferLost { r#ref, held: None })
        .unwrap();
    pool.step(PoolStep::TransferRefused { r#ref }).unwrap();
    let sent = pool.deposits[&r#ref].payout.sent.unwrap();
    assert!(sent.refused && sent.lost.0);
    assert!(!pool.deposits[&r#ref].payout.nothing_went_out());
    // asked again: still lost
    pool.step(asked(r#ref)).unwrap();
    assert!(pool.deposits[&r#ref].payout.sent.unwrap().lost.0);
    pool.step(PoolStep::TransferRefused { r#ref }).unwrap();
    hold(&mut pool, r#ref);
    assert!(pool
        .step(resolved(r#ref, DepositResolution::Refund, None))
        .is_err());
    assert!(pool
        .step(resolved(r#ref, DepositResolution::Retry, Some(7)))
        .is_err());
    pool.step(resolved(r#ref, DepositResolution::Retry, None))
        .unwrap();
}

/// The review (2026-10-24): the fold does not trust the door for a retry's dedup pair. A
/// fresh instant is exactly the line's own time; a retry with its own pair (its transfer may
/// have landed) leaves its next drive inside the ledger's window less the margin.
#[test]
fn a_retrys_dedup_instant_is_the_lines_time_inside_the_window() {
    use std::time::Duration;
    let mut pool = funded(usdc(1_000));
    let line = Timestamp::from_nanos(1_800_000_000_000_000_000);
    let retry_at = |at: Timestamp| PoolStep::DepositResolved {
        r#ref: PoolRef::new([1; 32]),
        resolution: DepositResolution::Retry,
        retry_at: Some(at),
    };
    assert_eq!(pool.ledger.admits_at(&retry_at(line), &pool, line), Ok(()));
    let later = line.checked_add(Duration::from_secs(1)).unwrap();
    assert!(pool
        .ledger
        .admits_at(&retry_at(later), &pool, line)
        .is_err());
    let earlier = Timestamp::from_nanos(line.as_nanos() - 1);
    assert!(pool
        .ledger
        .admits_at(&retry_at(earlier), &pool, line)
        .is_err());
    // its own pair: the payout started at 5 ns, its transfer asked, its answer unknown
    let landed = started(&mut pool, 2, 2);
    pool.step(asked(landed)).unwrap();
    hold(&mut pool, landed);
    let own = PoolStep::DepositResolved {
        r#ref: landed,
        resolution: DepositResolution::Retry,
        retry_at: None,
    };
    let edge = Timestamp::from_nanos(
        5 + (crate::pool::DEDUP_WINDOW - crate::pool::RETRY_MARGIN).as_nanos() as u64,
    );
    assert!(pool.ledger.admits_at(&own, &pool, edge).is_err());
    let inside = Timestamp::from_nanos(edge.as_nanos() - 1);
    assert_eq!(pool.ledger.admits_at(&own, &pool, inside), Ok(()));
}

/// The review (2026-10-24): an ask on other terms is refused while an earlier one may have
/// landed, and one whose answer was once lost may have, even if refused since.
#[test]
fn an_ask_on_other_terms_is_refused_after_a_lost_answer_even_once_refused() {
    let mut pool = funded(usdc(1_000));
    let r#ref = started(&mut pool, 1, 1);
    pool.step(asked(r#ref)).unwrap();
    pool.step(PoolStep::TransferLost { r#ref, held: None })
        .unwrap();
    pool.step(PoolStep::TransferRefused { r#ref }).unwrap();
    let mut other = asked(r#ref);
    if let PoolStep::TransferAsked { amount, .. } = &mut other {
        *amount = usdc(1);
    }
    assert_eq!(pool.step(other), Err(PoolStepError::AskedOnOtherTerms));
    // on its own terms it is asked again
    pool.step(asked(r#ref)).unwrap();
}

/// How many variants `PoolStep` has: [`place`] gives each its own place below it.
const STEP_COUNT: usize = 72;

/// Each step's place in `PoolStep`'s order. The match has no catch-all, so a step added to
/// `PoolStep` does not compile here until it has a place, and a sample in [`every_step`].
fn place(step: &PoolStep) -> usize {
    match step {
        PoolStep::SettingsSet { .. } => 0,
        PoolStep::Registered { .. } => 1,
        PoolStep::Expired { .. } => 2,
        PoolStep::Arrived { .. } => 3,
        PoolStep::Unattributed { .. } => 4,
        PoolStep::PayoutStarted { .. } => 5,
        PoolStep::Paid { .. } => 6,
        PoolStep::FellBack { .. } => 7,
        PoolStep::Minted { .. } => 8,
        PoolStep::StakedIcp { .. } => 9,
        PoolStep::WithdrawnIcp { .. } => 10,
        PoolStep::AttemptFailed { .. } => 11,
        PoolStep::SwapDeclined { .. } => 12,
        PoolStep::SwapBegun { .. } => 13,
        PoolStep::Swapped { .. } => 14,
        PoolStep::StakeStarted { .. } => 15,
        PoolStep::StakeFailed { .. } => 16,
        PoolStep::WithdrawStarted { .. } => 17,
        PoolStep::IcpChecked { .. } => 18,
        PoolStep::SwapApproved { .. } => 19,
        PoolStep::SwapUndone { .. } => 20,
        PoolStep::PayoutSettingsSet { .. } => 21,
        PoolStep::PayoutOpened { .. } => 22,
        PoolStep::PayoutAmountKnown { .. } => 23,
        PoolStep::PayoutQueued { .. } => 24,
        PoolStep::PayoutWaiting { .. } => 25,
        PoolStep::PayoutSigned { .. } => 26,
        PoolStep::PayoutPaid { .. } => 27,
        PoolStep::PayoutReverted { .. } => 28,
        PoolStep::PayoutUndoing { .. } => 29,
        PoolStep::PayoutUndone { .. } => 30,
        PoolStep::IcpRoutesSet { .. } => 31,
        PoolStep::ApproveSigned { .. } => 32,
        PoolStep::PayoutPulled { .. } => 33,
        PoolStep::PayoutStopped { .. } => 34,
        PoolStep::PayoutRelisted { .. } => 35,
        PoolStep::PayoutRefunded { .. } => 36,
        PoolStep::OrphanBooked { .. } => 37,
        PoolStep::OrphanRefundSigned { .. } => 38,
        PoolStep::OrphanRefundClosed { .. } => 39,
        PoolStep::TermsClaimed { .. } => 40,
        PoolStep::PayoutDelivered { .. } => 41,
        PoolStep::PayoutUnfilled { .. } => 42,
        PoolStep::PayoutResolved { .. } => 43,
        PoolStep::PayoutFitWaited { .. } => 44,
        PoolStep::PayoutFillEarly { .. } => 45,
        PoolStep::DepositResolved { .. } => 46,
        PoolStep::StakeResolved { .. } => 47,
        PoolStep::TransferAsked { .. } => 48,
        PoolStep::TransferRefused { .. } => 49,
        PoolStep::TransferLost { .. } => 50,
        PoolStep::MoveSettingsSet { .. } => 51,
        PoolStep::MoveOpened { .. } => 52,
        PoolStep::MoveProposed { .. } => 53,
        PoolStep::MoveRouted { .. } => 54,
        PoolStep::MoveSigned { .. } => 55,
        PoolStep::MoveBurned { .. } => 56,
        PoolStep::MoveReverted { .. } => 57,
        PoolStep::MoveWithdrawing { .. } => 58,
        PoolStep::MoveWithdrawn { .. } => 59,
        PoolStep::MoveRefused { .. } => 60,
        PoolStep::MoveLanded { .. } => 61,
        PoolStep::MoveSweeping { .. } => 62,
        PoolStep::MoveArrived { .. } => 63,
        PoolStep::MoveStuck { .. } => 64,
        PoolStep::MoveRetried { .. } => 65,
        PoolStep::MoveLost { .. } => 66,
        PoolStep::MoveRefunded { .. } => 67,
        PoolStep::EcoRescueSigned { .. } => 68,
        PoolStep::MoveUnfilled { .. } => 69,
        PoolStep::MoveReimbursed { .. } => 70,
        PoolStep::SweepSigned { .. } => 71,
    }
}

/// One sample of every pool step, whatever its fields: what the routing test sends through
/// `admits` and `apply`.
fn every_step() -> Vec<PoolStep> {
    use crate::numeric::Nonce;
    use crate::pool::payout::{IcpSwapTerms, PayoutKind, PayoutResolution, PayoutSettings};
    use crate::pool::payout::{PayoutTerms, PinnedOrder, Undo};
    use crate::pool::{
        IcpCheck, IcpRoutes, MoveDirection, MoveRoute, PayoutPath, PoolAccount, RebalanceSettings,
    };

    let r#ref = PoolRef::new([0x51; 32]);
    let id = PoolOpId::new([0x52; 32]);
    let at = Timestamp::from_nanos(1);
    let amount = usdc(1);
    let block = BlockNumber::new(9);
    let tx_hash = TxHash::new([0x53; 32]);
    let nonce = Nonce::new(1);
    let credit = CreditKey {
        chain: ChainId::BASE,
        tx: tx_hash,
        log_index: 0,
    };
    let attempt = PayoutAttempt {
        created_at: at,
        memo: [0x54; 32],
        path: PayoutPath::Ckusdc,
        amount: None,
        token: None,
    };
    let order = PinnedOrder {
        order_id: [0x55; 32],
        order: vec![0x55],
        gas_limit: 1,
        deadline_s: 1,
    };
    let terms = PayoutTerms {
        dst_chain: ChainId::BASE,
        recipient: payer(),
        token_out: payer(),
        min_out: amount,
        deadline: at,
    };
    let swap_terms = IcpSwapTerms {
        token_in: ckusdc(),
        amount_in: amount,
        from: recipient(1),
        min_usdc: amount,
        premium: TokenAmount::ZERO,
        network_fee: TokenAmount::ZERO,
        refund_to: recipient(2),
        slippage_bps: None,
        planned: None,
    };
    let owner = PoolAccount::Icp(recipient(3));
    let why = "why".to_string();
    vec![
        PoolStep::SettingsSet {
            settings: settings(),
        },
        PoolStep::Registered {
            r#ref,
            deposit: swap(1, usdc(10), usdc(1)),
        },
        PoolStep::Expired { r#ref },
        PoolStep::Arrived {
            r#ref,
            credit,
            amount,
            block,
        },
        PoolStep::Unattributed {
            credit,
            amount,
            src_payer: None,
            why: why.clone(),
            block,
            r#ref: None,
            awaiting_until: None,
        },
        PoolStep::PayoutStarted { r#ref, attempt },
        PoolStep::Paid {
            r#ref,
            token: ckusdc(),
            amount,
            block: 1,
            spent: amount,
        },
        PoolStep::FellBack {
            r#ref,
            why: why.clone(),
            amount: None,
            token: None,
        },
        PoolStep::Minted {
            r#ref,
            owner,
            shares: Shares::new(1),
        },
        PoolStep::StakedIcp {
            id,
            owner,
            amount,
            move_fee: TokenAmount::ZERO,
            shares: Shares::new(1),
            block: 1,
        },
        PoolStep::WithdrawnIcp {
            id,
            owner: recipient(3),
            burned: Shares::new(1),
            usdc: amount,
            token: ckusdc(),
            paid: amount,
            block: 1,
            spent: amount,
        },
        PoolStep::AttemptFailed {
            r#ref,
            why: why.clone(),
        },
        PoolStep::SwapDeclined {
            r#ref,
            why: why.clone(),
            leg: None,
        },
        PoolStep::SwapBegun {
            r#ref,
            before: None,
            leg: None,
            amount_in: None,
        },
        PoolStep::Swapped {
            r#ref,
            out: amount,
            send: None,
            leg: None,
        },
        PoolStep::StakeStarted {
            id,
            owner,
            from: recipient(4),
            amount,
            created_at: at,
            move_fee: TokenAmount::ZERO,
        },
        PoolStep::StakeFailed {
            id,
            why: why.clone(),
        },
        PoolStep::WithdrawStarted {
            id,
            owner: recipient(3),
            burned: Shares::new(1),
            usdc: amount,
            to: recipient(5),
            token_out: ckusdc(),
            min_out: 1,
            attempt,
            move_fee: TokenAmount::ZERO,
        },
        PoolStep::IcpChecked {
            check: IcpCheck {
                observed: amount,
                expected: amount,
                shortfall: false,
            },
        },
        PoolStep::SwapApproved { r#ref, leg: None },
        PoolStep::SwapUndone {
            r#ref,
            cost: amount,
            withdrawn: amount,
            leg: None,
        },
        PoolStep::PayoutSettingsSet {
            settings: PayoutSettings::default(),
        },
        PoolStep::PayoutOpened {
            id: 0,
            kind: Box::new(PayoutKind::IcpSwap(swap_terms)),
            terms,
            undo: Undo::Refund {
                to: swap_terms.refund_to,
            },
            plan: None,
        },
        PoolStep::PayoutAmountKnown {
            id: 0,
            amount,
            received: None,
        },
        PoolStep::PayoutQueued { id: 0 },
        PoolStep::PayoutWaiting { id: 0 },
        PoolStep::PayoutSigned {
            id: 0,
            order: order.clone(),
            chain_id: ChainId::BASE,
            nonce,
            tx_hash,
            raw_tx: vec![0x02],
        },
        PoolStep::PayoutPaid {
            id: 0,
            tx_hash,
            block,
            intent: None,
        },
        PoolStep::PayoutReverted { id: 0, block: None },
        PoolStep::PayoutUndoing {
            id: 0,
            why: why.clone(),
        },
        PoolStep::PayoutUndone {
            id: 0,
            why: why.clone(),
            block: None,
        },
        PoolStep::IcpRoutesSet {
            routes: IcpRoutes::default(),
        },
        PoolStep::ApproveSigned {
            chain_id: ChainId::BASE,
            nonce,
            tx_hash,
            raw_tx: vec![0x02],
        },
        PoolStep::PayoutPulled {
            id: 0,
            block: 1,
            attempt: None,
        },
        PoolStep::PayoutStopped {
            id: 0,
            why: why.clone(),
        },
        PoolStep::PayoutRelisted { id: 0 },
        PoolStep::PayoutRefunded {
            id: 0,
            credit,
            amount,
            block,
        },
        PoolStep::OrphanBooked { key: credit },
        PoolStep::OrphanRefundSigned {
            key: credit,
            to: payer(),
            sent: amount,
            chain_id: ChainId::BASE,
            nonce,
            tx_hash,
            raw_tx: vec![0x02],
        },
        PoolStep::OrphanRefundClosed {
            key: credit,
            success: true,
        },
        PoolStep::TermsClaimed {
            credit,
            refused: None,
        },
        PoolStep::PayoutDelivered { id: 0, block },
        PoolStep::PayoutUnfilled { id: 0, block },
        PoolStep::PayoutResolved {
            id: 0,
            resolution: PayoutResolution::Refund,
            refund_at: None,
        },
        PoolStep::PayoutFitWaited {
            id: 0,
            why: why.clone(),
        },
        PoolStep::PayoutFillEarly { id: 0 },
        PoolStep::DepositResolved {
            r#ref,
            resolution: DepositResolution::Retry,
            retry_at: None,
        },
        PoolStep::StakeResolved {
            id,
            resolution: StakeResolution::NotPulled,
        },
        PoolStep::TransferAsked {
            r#ref,
            token: PrincipalBytes(ckusdc()),
            amount,
            spent: amount,
        },
        PoolStep::TransferRefused { r#ref },
        PoolStep::TransferLost { r#ref, held: None },
        PoolStep::MoveSettingsSet {
            settings: RebalanceSettings::default(),
        },
        PoolStep::MoveOpened {
            id: 0,
            direction: MoveDirection::BaseToIcp,
            amount,
            urgent: false,
            opened_at: at,
            window_ends: None,
            own_cost: TokenAmount::ZERO,
        },
        PoolStep::MoveProposed {
            id: 0,
            order: order.clone(),
            fee: amount,
        },
        PoolStep::MoveRouted {
            id: 0,
            route: MoveRoute::Own {
                max_fee: amount,
                fast: false,
            },
            order,
            scan_from: None,
        },
        PoolStep::MoveSigned {
            id: 0,
            chain_id: ChainId::BASE,
            nonce,
            tx_hash,
            raw_tx: vec![0x02],
        },
        PoolStep::MoveBurned {
            id: 0,
            tx_hash,
            block,
            intent: None,
        },
        PoolStep::MoveReverted { id: 0, block: None },
        PoolStep::MoveWithdrawing {
            id: 0,
            created_at: at,
        },
        PoolStep::MoveWithdrawn {
            id: 0,
            withdrawal_id: 1,
            fee: amount,
        },
        PoolStep::MoveRefused {
            id: 0,
            why: why.clone(),
            fee: amount,
        },
        PoolStep::MoveLanded {
            id: 0,
            tx_hash,
            block,
            amount,
            window_ends: None,
        },
        PoolStep::MoveSweeping {
            id: 0,
            created_at: at,
            amount,
            credited: amount,
        },
        PoolStep::MoveArrived {
            id: 0,
            credited: amount,
            credit: None,
            block: None,
            ledger_block: None,
        },
        PoolStep::MoveStuck {
            id: 0,
            why: why.clone(),
        },
        PoolStep::MoveRetried { id: 0 },
        PoolStep::MoveLost { id: 0, why },
        PoolStep::MoveRefunded {
            id: 0,
            credit,
            amount,
            block,
        },
        PoolStep::EcoRescueSigned {
            order_id: [0x56; 32],
            chain_id: ChainId::BASE,
            nonce,
            tx_hash,
            raw_tx: vec![0x02],
        },
        PoolStep::MoveUnfilled { id: 0 },
        PoolStep::MoveReimbursed {
            id: 0,
            amount,
            block: 1,
        },
        PoolStep::SweepSigned {
            id: 0,
            chain_id: ChainId::BASE,
            nonce,
            tx_hash,
            raw_tx: vec![0x02],
        },
    ]
}

/// The review (2026-10-24): every step reaches the family with its arm, in `admits` and in
/// `apply`. A family refuses a step it has no arm for, and its changes for one are a bug, so
/// a step sent to the wrong family fails here, never admitted unchecked in production.
#[test]
fn every_step_is_routed_to_the_family_with_its_arm() {
    let steps = every_step();
    let places: BTreeSet<usize> = steps.iter().map(place).collect();
    assert_eq!(steps.len(), STEP_COUNT);
    assert_eq!(places, (0..STEP_COUNT).collect());
    let mut pool = Pool::default();
    pool.step(PoolStep::SettingsSet {
        settings: settings(),
    })
    .unwrap();
    for step in &steps {
        assert_ne!(
            pool.ledger.admits(step, &pool),
            Err(unrouted(step)),
            "{} is admitted by the family with its arm",
            step.name()
        );
        // a step sent to a family with no arm for it would panic here
        pool.ledger
            .clone()
            .apply_at(step, &pool, Timestamp::from_nanos(1), 1);
    }
}

/// An ICP stake's move fee is Swapic's: when the pull lands the stake's ckUSDC goes free on
/// ICP less the fee, which goes to the ICP fee line, and shares are minted on the rest. A
/// landed pull naming another fee than its start is refused.
#[test]
fn an_icp_stake_books_its_move_fee_to_swapic() {
    let mut pool = funded(usdc(1_000));
    let owner = PoolAccount::Icp(recipient(210));
    let id = PoolOpId::new([0x71; 32]);
    let fee = usdc(1);
    pool.step(PoolStep::StakeStarted {
        id,
        owner,
        from: recipient(210),
        amount: usdc(100),
        created_at: Timestamp::from_nanos(1),
        move_fee: fee,
    })
    .unwrap();
    let assets = pool.ledger.total_assets();
    let free = pool.ledger.pockets.icp.free;
    let fees = pool.ledger.lines().fees_icp;
    let shares = pool.ledger.mint_now(usdc(99)).unwrap();
    let landed = |move_fee| PoolStep::StakedIcp {
        id,
        owner,
        amount: usdc(100),
        move_fee,
        shares,
        block: 3,
    };
    assert_eq!(
        pool.step(landed(TokenAmount::ZERO)),
        Err(PoolStepError::Mismatch)
    );
    pool.step(landed(fee)).unwrap();
    assert_eq!(
        pool.ledger.pockets.icp.free,
        free.checked_add(usdc(99)).unwrap()
    );
    assert_eq!(pool.ledger.lines().fees_icp, fees.checked_add(fee).unwrap());
    assert_eq!(
        pool.ledger.total_assets(),
        assets.checked_add(usdc(99)).unwrap()
    );
    assert_eq!(pool.ledger.stake_room, TokenAmount::ZERO);
    // a fee as large as the stake is refused at the start
    assert!(pool
        .step(PoolStep::StakeStarted {
            id: PoolOpId::new([0x72; 32]),
            owner,
            from: recipient(210),
            amount: usdc(5),
            created_at: Timestamp::from_nanos(2),
            move_fee: usdc(5),
        })
        .is_err());
}

/// An ICP withdrawal of `usdc` costs the stakers all of it: its move fee goes to the ICP fee
/// line at once, and only the rest is reserved and owed for the payout, which spends it.
#[test]
fn an_icp_withdrawal_reserves_all_but_its_move_fee() {
    let mut pool = funded(usdc(1_000));
    let owner = recipient(200);
    let burned = Shares::burn_for(
        usdc(100),
        pool.ledger.total_assets(),
        pool.ledger.total_shares,
    )
    .unwrap();
    let assets = pool.ledger.total_assets();
    let icp = pool.ledger.pockets.icp;
    let fees = pool.ledger.lines().fees_icp;
    let fee = usdc(2);
    let id = PoolOpId::new([0x73; 32]);
    pool.step(PoolStep::WithdrawStarted {
        id,
        owner,
        burned,
        usdc: usdc(100),
        to: owner,
        token_out: ckusdc(),
        min_out: 0,
        attempt: PayoutAttempt {
            created_at: Timestamp::from_nanos(9),
            memo: [0x73; 32],
            path: PayoutPath::Ckusdc,
            amount: None,
            token: None,
        },
        move_fee: fee,
    })
    .unwrap();
    assert_eq!(
        pool.ledger.total_assets(),
        assets.checked_sub(usdc(100)).unwrap()
    );
    assert_eq!(
        pool.ledger.pockets.icp.free,
        icp.free.checked_sub(usdc(100)).unwrap()
    );
    assert_eq!(
        pool.ledger.pockets.icp.reserved,
        icp.reserved.checked_add(usdc(98)).unwrap()
    );
    assert_eq!(pool.ledger.lines().fees_icp, fees.checked_add(fee).unwrap());
    assert!(matches!(
        pool.ledger.pending.get(&id),
        Some(PendingOp::Withdraw { usdc: budget, .. }) if *budget == usdc(98)
    ));
    // what the ICP account should hold is unchanged until the payout spends its budget
    assert_eq!(
        pool.ledger.icp_held(),
        icp.free
            .checked_add(icp.reserved)
            .unwrap()
            .checked_add(fees)
            .unwrap()
    );
}
