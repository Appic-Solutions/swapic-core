use super::*;
use crate::evm::EvmAddress;
use crate::hash::TxHash;
use crate::pool::{pool_ref, IcpAccount, LedgerFee, PayoutAttempt, PoolRail, CKUSDC_LEDGER};

/// The maps the storage layer keeps beside the ledger, on the heap, and the steps applied the
/// way the storage layer applies them: `admits`, then every effect.
#[derive(Default)]
pub(crate) struct Pool {
    pub ledger: PoolLedger,
    pub deposits: BTreeMap<PoolRef, Registered>,
    pub shares: BTreeMap<OwnerKey, ShareLine>,
    pub credited: BTreeSet<CreditKey>,
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
        Ok(())
    }
}

fn ckusdc() -> Principal {
    Principal::from_text(CKUSDC_LEDGER).unwrap()
}

fn usdc(whole: u64) -> TokenAmount {
    TokenAmount::from(whole * 1_000_000)
}

const FEE: u64 = 10_000;

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

fn payer() -> EvmAddress {
    "0xf00EBA8E5Fd231b526ADFAAa68B4d0ede72A89AC"
        .parse()
        .unwrap()
}

fn recipient(byte: u8) -> IcpAccount {
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
        rebalance_share: TokenAmount::ZERO,
        src_chain: ChainId::ARBITRUM,
        src_payer: payer(),
        rail: PoolRail::Cctp,
        expires_at: Timestamp::from_nanos(1_000),
        salt: [byte; 32],
    }
}

fn stake(byte: u8, min_usdc: TokenAmount, rebalance_share: TokenAmount) -> Registration {
    Registration {
        kind: DepositKind::Stake {
            owner: PoolAccount::Icp(recipient(byte)),
        },
        min_usdc,
        premium: TokenAmount::ZERO,
        rebalance_share,
        src_chain: ChainId::BASE,
        src_payer: payer(),
        rail: PoolRail::SameChain,
        expires_at: Timestamp::from_nanos(1_000),
        salt: [byte; 32],
    }
}

fn credit(log: u64) -> CreditKey {
    CreditKey {
        chain: ChainId::BASE,
        tx: TxHash::new([0xab; 32]),
        log_index: log,
    }
}

fn register(pool: &mut Pool, terms: Registration) -> PoolRef {
    let r#ref = terms.pool_ref().unwrap();
    pool.step(PoolStep::Registered {
        r#ref,
        deposit: terms,
    })
    .unwrap();
    r#ref
}

/// A pool with settings and `icp` of ckUSDC staked on ICP by `recipient(200)`.
fn funded(icp: TokenAmount) -> Pool {
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
    })
    .unwrap();
    let shares = pool.ledger.mint_now(icp).unwrap();
    pool.step(PoolStep::StakedIcp {
        id,
        owner,
        amount: icp,
        rebalance_share: TokenAmount::ZERO,
        shares,
        block: 1,
    })
    .unwrap();
    pool
}

#[test]
fn a_registration_above_a_cap_or_below_the_premium_is_refused() {
    let pool = funded(usdc(5_000));
    let good = swap(1, usdc(100), usdc(1));
    assert_eq!(check_terms(&pool.ledger, &good), Ok(()));
    // above the per-swap cap
    assert_eq!(
        check_terms(&pool.ledger, &swap(1, usdc(1_001), usdc(2))),
        Err(RegisterRefusal::AboveSwapCap { cap: usdc(1_000) })
    );
    // 10 bps of 100 USDC is 0.1, and the floor rounds up: 100.000001 needs 100_001
    assert_eq!(
        check_terms(
            &pool.ledger,
            &swap(1, usdc(100), TokenAmount::from(99_999_u32))
        ),
        Err(RegisterRefusal::PremiumTooLow {
            least: TokenAmount::from(100_000_u32)
        })
    );
    assert_eq!(
        check_terms(
            &pool.ledger,
            &swap(
                1,
                usdc(100).checked_add(TokenAmount::ONE).unwrap(),
                TokenAmount::from(100_000_u32)
            )
        ),
        Err(RegisterRefusal::PremiumTooLow {
            least: TokenAmount::from(100_001_u32)
        })
    );
    // a small swap's premium never goes below the ledger fee its payout costs
    assert_eq!(
        check_terms(
            &pool.ledger,
            &swap(1, usdc(2), TokenAmount::from(2_000_u32))
        ),
        Err(RegisterRefusal::PremiumTooLow {
            least: TokenAmount::from(FEE)
        })
    );
    // a stake that would take the pool past its TVL cap
    assert_eq!(
        check_terms(&pool.ledger, &stake(2, usdc(5_001), TokenAmount::ZERO)),
        Err(RegisterRefusal::AboveTvlCap {
            cap: usdc(10_000),
            would_be: usdc(10_001)
        })
    );
    assert_eq!(
        check_terms(&pool.ledger, &stake(2, usdc(5_000), TokenAmount::ZERO)),
        Ok(())
    );
    // a ckUSDC payout below the recipient's own minimum
    let mut short = swap(1, usdc(100), usdc(1));
    short.kind = DepositKind::Swap {
        recipient: recipient(1),
        token_out: ckusdc(),
        min_out: 99_000_001,
    };
    assert_eq!(
        check_terms(&pool.ledger, &short),
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
        check_terms(&pool.ledger, &unlisted),
        Err(RegisterRefusal::TokenNotListed(other))
    );
}

#[test]
fn the_ref_and_the_fee_it_commits_are_checked() {
    let pool = funded(usdc(5_000));
    let terms = swap(1, usdc(100), usdc(1));
    let r#ref = terms.pool_ref().unwrap();
    assert_eq!(check_ref(&pool.ledger, r#ref, &terms, &pool), Ok(()));
    // the quoter registers a larger premium than the user's app committed to
    let greedy = Registration {
        premium: usdc(2),
        ..terms.clone()
    };
    assert_eq!(
        check_ref(&pool.ledger, r#ref, &greedy, &pool),
        Err(RegisterRefusal::BadRef)
    );
    // a swap with a rebalancing share the ref does not commit
    let both = Registration {
        rebalance_share: TokenAmount::ONE,
        ..terms.clone()
    };
    let both_ref = both.pool_ref().unwrap();
    assert!(matches!(
        check_ref(&pool.ledger, both_ref, &both, &pool),
        Err(RegisterRefusal::NotAllowed(_))
    ));
    // closed for swaps
    let mut closed = funded(usdc(5_000));
    closed.ledger.settings.as_mut().unwrap().open_for_swaps = false;
    assert_eq!(
        check_ref(&closed.ledger, r#ref, &terms, &closed),
        Err(RegisterRefusal::ClosedForSwaps)
    );
    // a stake registers while closed for swaps
    let staked = stake(3, usdc(10), TokenAmount::ZERO);
    assert_eq!(
        check_ref(&closed.ledger, staked.pool_ref().unwrap(), &staked, &closed),
        Ok(())
    );
}

#[test]
fn the_open_limit_and_the_held_cap_are_kept() {
    let mut pool = funded(usdc(1_000));
    for byte in 1..=3 {
        let terms = swap(byte, usdc(10), usdc(1));
        check_terms(&pool.ledger, &terms).unwrap();
        register(&mut pool, terms);
    }
    // a fourth from the same payer
    assert_eq!(
        check_terms(&pool.ledger, &swap(4, usdc(10), usdc(1))),
        Err(RegisterRefusal::NotAllowed(
            "too many open deposits".to_string()
        ))
    );
    // 27 of 1000 held; half of the pocket may be: 500 - 27 = 473 more, 474 is too much
    let mut other = swap(4, usdc(475), usdc(1));
    other.src_payer = "0x1111111111111111111111111111111111111111"
        .parse()
        .unwrap();
    assert_eq!(
        check_terms(&pool.ledger, &other),
        Err(RegisterRefusal::PocketShort)
    );
    other.min_usdc = usdc(474);
    assert_eq!(check_terms(&pool.ledger, &other), Ok(()));
}

/// A good registration reserves `min_usdc - premium` of the ICP pocket; its expiry releases
/// it; a stake holds TVL room and nothing on ICP.
#[test]
fn a_registration_holds_and_its_expiry_releases() {
    let mut pool = funded(usdc(1_000));
    let r#ref = register(&mut pool, swap(1, usdc(100), usdc(1)));
    assert_eq!(pool.ledger.pockets.icp.reserved, usdc(99));
    assert_eq!(pool.ledger.pockets.icp.free, usdc(901));
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
    assert_eq!(pool.ledger.pockets.icp.reserved, TokenAmount::ZERO);
    assert_eq!(pool.ledger.pockets.icp.free, usdc(1_000));
    assert_eq!(pool.ledger.open_of(&PoolAccount::Evm(payer()).key()), 0);
    assert!(pool.ledger.open.is_empty());
    assert_eq!(pool.deposits[&r#ref].status, DepositStatus::Expired);
    assert!(pool.step(PoolStep::Expired { r#ref }).is_err());

    let staked = register(&mut pool, stake(2, usdc(50), TokenAmount::ZERO));
    assert_eq!(pool.ledger.stake_room, usdc(50));
    assert_eq!(pool.ledger.pockets.icp.reserved, TokenAmount::ZERO);
    pool.step(PoolStep::Expired { r#ref: staked }).unwrap();
    assert_eq!(pool.ledger.stake_room, TokenAmount::ZERO);
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
/// closes its registration, releases its hold, and is no pool asset.
#[test]
fn a_late_arrival_is_unattributed_and_releases_its_hold() {
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
    // in the pockets twice (Base arrival, ICP reservation), owed once
    assert_eq!(pool.ledger.owed, usdc(99));
    assert_eq!(pool.ledger.total_assets(), usdc(1_001));
    assert!(pool.ledger.paying.contains(&r#ref));
    let attempt = PayoutAttempt {
        created_at: Timestamp::from_nanos(5),
        memo: r#ref.into_bytes(),
        path: PayoutPath::Ckusdc,
    };
    pool.step(PoolStep::PayoutStarted { r#ref, attempt })
        .unwrap();
    assert_eq!(pool.deposits[&r#ref].status, DepositStatus::Paying);
    pool.step(PoolStep::Paid {
        r#ref,
        token: ckusdc(),
        amount: usdc(99),
        block: 42,
        spent: usdc(99).checked_add(TokenAmount::from(FEE)).unwrap(),
    })
    .unwrap();
    assert_eq!(pool.deposits[&r#ref].status, DepositStatus::Paid);
    assert_eq!(pool.ledger.owed, TokenAmount::ZERO);
    assert_eq!(pool.ledger.pockets.icp.reserved, TokenAmount::ZERO);
    // 1000 - 99 - the fee on ICP, 100 on Base: the premium less the fee earned
    assert_eq!(
        pool.ledger.pockets.icp.free,
        usdc(901).checked_sub(TokenAmount::from(FEE)).unwrap()
    );
    assert_eq!(pool.ledger.pockets.base.free, usdc(100));
    assert_eq!(
        pool.ledger.total_assets(),
        usdc(1_001).checked_sub(TokenAmount::from(FEE)).unwrap()
    );
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
    assert_eq!(pool.ledger.total_assets(), usdc(1_100));
    // worth what came in less its share, and a part of that share
    let value = Shares::value_of(shares, pool.ledger.total_assets(), pool.ledger.total_shares);
    assert!(value >= usdc(99).checked_sub(TokenAmount::ONE).unwrap());
    assert!(value < usdc(100));
}

/// Review Focus 3, through the fold: a withdrawal while a swap's payout is unpaid takes no
/// more than its shares were worth before the swap arrived and its premium.
#[test]
fn a_withdrawal_during_an_unpaid_payout_takes_no_more_than_its_value() {
    let mut pool = funded(usdc(1_000));
    let owner = recipient(200);
    let r#ref = register(&mut pool, swap(1, usdc(100), usdc(1)));
    pool.step(PoolStep::Arrived {
        r#ref,
        credit: credit(1),
        amount: usdc(100),
        block: BlockNumber::new(10),
    })
    .unwrap();
    let owned = pool.shares_of(&PoolAccount::Icp(owner).key());
    let worth = Shares::value_of(owned, pool.ledger.total_assets(), pool.ledger.total_shares);
    assert_eq!(
        worth,
        usdc(1_001),
        "what was staked and the premium, not the arrival"
    );
    // asking for more than that burns more than is owned
    let too_much = worth.checked_add(TokenAmount::ONE).unwrap();
    assert_eq!(
        Shares::burn_for(
            too_much,
            pool.ledger.total_assets(),
            pool.ledger.total_shares
        ),
        None
    );
    let started = |usdc: TokenAmount, burned: Shares| PoolStep::WithdrawStarted {
        id: PoolOpId::new([0x77; 32]),
        owner,
        burned,
        usdc,
        to: owner,
        token_out: ckusdc(),
        min_out: 0,
        attempt: PayoutAttempt {
            created_at: Timestamp::from_nanos(9),
            memo: [0x77; 32],
            path: PayoutPath::Ckusdc,
        },
    };
    // the ICP pocket holds 901 free: a withdrawal is paid from it, not from Base
    assert!(matches!(
        pool.step(started(usdc(1_001), owned)),
        Err(PoolStepError::PocketShort { .. })
    ));
    // too few shares burned for what is asked
    assert!(matches!(
        pool.step(started(usdc(500), Shares::new(owned.get() / 3))),
        Err(PoolStepError::BurnTooSmall { .. })
    ));
    let burned = Shares::burn_for(
        usdc(500),
        pool.ledger.total_assets(),
        pool.ledger.total_shares,
    )
    .unwrap();
    pool.step(started(usdc(500), burned)).unwrap();
    assert_eq!(pool.ledger.pockets.icp.free, usdc(401));
    assert_eq!(pool.ledger.pockets.icp.reserved, usdc(599));
    assert_eq!(pool.ledger.owed, usdc(599));
    // the price did not move for what is left
    let left = pool.shares_of(&PoolAccount::Icp(owner).key());
    assert_eq!(
        Shares::value_of(left, pool.ledger.total_assets(), pool.ledger.total_shares),
        usdc(501)
    );
    pool.step(PoolStep::WithdrawnIcp {
        id: PoolOpId::new([0x77; 32]),
        owner,
        burned,
        usdc: usdc(500),
        token: ckusdc(),
        paid: usdc(500).checked_sub(TokenAmount::from(FEE)).unwrap(),
        block: 3,
        spent: usdc(500),
    })
    .unwrap();
    assert!(pool.ledger.pending.is_empty());
    assert_eq!(pool.ledger.pockets.icp.reserved, usdc(99));
    assert_eq!(pool.ledger.owed, usdc(99));
}

/// A payout that keeps failing stops for a human at the tenth failure, and the drive's index
/// lets go of it.
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
    assert!(pool.ledger.paying.is_empty());
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
        },
    })
    .unwrap();
    let declined = || PoolStep::SwapDeclined {
        r#ref,
        why: "below".into(),
    };
    pool.step(declined()).unwrap();
    assert!(pool.step(declined()).is_err());
    pool.step(PoolStep::SwapBegun { r#ref }).unwrap();
    assert!(pool
        .step(PoolStep::FellBack {
            r#ref,
            why: "late".into()
        })
        .is_err());
    pool.step(PoolStep::Swapped {
        r#ref,
        out: TokenAmount::from(5_u8),
    })
    .unwrap();
    assert!(pool
        .step(PoolStep::Swapped {
            r#ref,
            out: TokenAmount::from(5_u8),
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
    };
    pool.step(PoolStep::PayoutStarted {
        r#ref: other,
        attempt,
    })
    .unwrap();
    pool.step(PoolStep::FellBack {
        r#ref: other,
        why: "below the minimum twice".into(),
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
            r#ref: pool_ref(&terms.kind, 1_000_000, terms.salt),
            deposit: terms,
        },
        PoolStep::IcpCheckFailed,
    ];
    for step in steps {
        assert_eq!(PoolStep::from_bytes(&step.to_bytes()), Some(step));
    }
}
