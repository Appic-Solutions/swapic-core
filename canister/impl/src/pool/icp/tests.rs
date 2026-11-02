use super::*;
use crate::storage::events::append_event_at;
use crate::storage::on_fresh_memory;
use crate::storage::pool::tests::{
    account, at, ckusdc, credit, funded_pool, register, swap_terms, usdc, FEE,
};
use crate::storage::pool::{ledger, record_at};
use types::books::{BalanceCheck, BookStep, PoolClosed};
use types::pool::IcpReadFailure;
use types::{BlockNumber, ChainId, EventType};

fn nobody(_: &Address) -> bool {
    false
}

/// The staker `funded_pool` planted: `account(200)`, 1000 USDC on ICP.
fn staker() -> Principal {
    account(200).owner
}

fn stake_request(amount: TokenAmount) -> StakeRequest {
    StakeRequest {
        caller: account(7).owner,
        from_subaccount: None,
        amount,
        owner: PoolAccount::Icp(account(7)),
        move_fee: TokenAmount::ZERO,
    }
}

fn withdrawal(usdc_out: TokenAmount) -> WithdrawRequest {
    WithdrawRequest {
        caller: staker(),
        from: None,
        usdc: usdc_out,
        max_shares: Shares::new(u128::MAX),
        token_out: ckusdc(),
        min_out: 0,
        to: account(201),
        move_fee: TokenAmount::ZERO,
    }
}

#[test]
fn a_stake_is_refused_when_it_should_be() {
    on_fresh_memory(|| {
        crate::storage::init();
        let check =
            |request: &StakeRequest| read_state(|state| check_stake(state, request, nobody));
        assert!(matches!(
            check(&stake_request(usdc(1))),
            Err(PoolError::Closed(_))
        ));
        funded_pool(usdc(9_000));
        assert_eq!(check(&stake_request(usdc(1_000))), Ok(()));
        assert!(matches!(
            check(&stake_request(TokenAmount::ZERO)),
            Err(PoolError::NotAllowed(_))
        ));
        assert_eq!(
            check(&stake_request(usdc(1_001))),
            Err(PoolError::AboveTvlCap {
                cap: Nat::from(usdc(10_000)),
                would_be: Nat::from(usdc(10_001)),
            })
        );
        let banned = account(7).owner.to_text();
        let sanctioned = |address: &Address| address.as_str() == banned;
        assert!(matches!(
            read_state(|state| check_stake(state, &stake_request(usdc(1)), sanctioned)),
            Err(PoolError::Sanctioned(_))
        ));
        // a stake on its way holds its room
        record_at(
            PoolStep::StakeStarted {
                id: types::hash::PoolOpId::new([1; 32]),
                owner: PoolAccount::Icp(account(7)),
                from: account(7),
                amount: usdc(600),
                created_at: at(2),
                move_fee: TokenAmount::ZERO,
            },
            at(2),
        )
        .unwrap();
        assert!(matches!(
            check(&stake_request(usdc(401))),
            Err(PoolError::AboveTvlCap { .. })
        ));
    });
}

#[test]
fn a_withdrawal_is_refused_when_it_should_be() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        let plan = |request: &WithdrawRequest, closed: Option<String>| {
            read_state(|state| plan_withdrawal(state, request, closed, nobody))
        };
        let WithdrawalPlan {
            burned,
            path,
            amount,
            ckusdc: ledger_of_ckusdc,
        } = plan(&withdrawal(usdc(400)), None).unwrap();
        assert_eq!(path, PayoutPath::Ckusdc);
        // the exact transfer its payout starts with: the amount less the ledger's fee
        assert_eq!(
            amount,
            usdc(400).checked_sub(TokenAmount::from(FEE)).unwrap()
        );
        assert_eq!(ledger_of_ckusdc, ckusdc());
        // 400 of 1000 burns 40% of the shares, rounded up
        let total = ledger().total_shares;
        assert_eq!(
            burned,
            Shares::burn_for(usdc(400), usdc(1_000), total).unwrap()
        );
        assert_eq!(burned.get(), total.get() / 10 * 4);
        // more than the caller owns, more than it allows
        assert!(matches!(
            plan(
                &WithdrawRequest {
                    caller: account(9).owner,
                    ..withdrawal(usdc(1))
                },
                None
            ),
            Err(PoolError::ShortShares { .. })
        ));
        assert_eq!(
            plan(
                &WithdrawRequest {
                    max_shares: Shares::new(burned.get() - 1),
                    ..withdrawal(usdc(400))
                },
                None
            ),
            Err(PoolError::ShortShares {
                have: Nat::from(burned.get() - 1),
                need: Nat::from(burned.get()),
            })
        );
        // closed, the pocket short, a token not listed, below min_out, zero, sanctioned
        assert_eq!(
            plan(&withdrawal(usdc(1)), Some("paused".into())),
            Err(PoolError::Closed("paused".into()))
        );
        assert_eq!(
            plan(&withdrawal(usdc(1_001)), None),
            Err(PoolError::NotAllowed(
                "the pool's assets cannot pay that".into()
            ))
        );
        let other = Principal::from_slice(&[9; 10]);
        assert_eq!(
            plan(
                &WithdrawRequest {
                    token_out: other,
                    ..withdrawal(usdc(1))
                },
                None
            ),
            Err(PoolError::TokenNotListed(other))
        );
        assert!(matches!(
            plan(
                &WithdrawRequest {
                    min_out: 1_000_000,
                    ..withdrawal(usdc(1))
                },
                None
            ),
            Err(PoolError::NotAllowed(_))
        ));
        assert!(matches!(
            plan(&withdrawal(TokenAmount::ZERO), None),
            Err(PoolError::NotAllowed(_))
        ));
        let banned = account(201).owner.to_text();
        assert!(matches!(
            read_state(|state| plan_withdrawal(
                state,
                &withdrawal(usdc(1)),
                None,
                |address: &Address| address.as_str() == banned
            )),
            Err(PoolError::Sanctioned(_))
        ));
        // a registration holds nothing; an arrived swap's queued payout does, and a
        // withdrawal waits behind it: 99 queued (its fees the user's), 902 is more than the
        // 901 behind it
        let r#ref = register(swap_terms(1, usdc(100), usdc(1), at(1_000)));
        assert_eq!(
            plan(&withdrawal(usdc(902)), None).map(|plan| plan.path),
            Ok(PayoutPath::Ckusdc)
        );
        record_at(
            PoolStep::Arrived {
                r#ref,
                credit: credit(1),
                amount: usdc(100),
                block: BlockNumber::new(5),
            },
            at(3),
        )
        .unwrap();
        assert_eq!(
            plan(&withdrawal(usdc(902)), None),
            Err(PoolError::Closed(POCKET_SHORT.into()))
        );
        assert!(plan(&withdrawal(usdc(901)), None).is_ok());
    });
}

/// Review fixes 8 and 11: a token path's withdrawal must more than cover every ckUSDC fee
/// its path can cost (the approval, the pool's pull, a withdrawal back, the fall back's
/// transfer), and its payout starts with the exact amount in: its amount less the two fees
/// the swap itself takes.
#[test]
fn a_token_path_withdrawal_covers_its_fees_and_fixes_its_amount_in() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        let token = Principal::from_slice(&[0x70; 10]);
        let pool = Principal::from_slice(&[0x71; 10]);
        let mut listed = crate::storage::pool::tests::settings();
        listed.icp_tokens = vec![types::pool::IcpToken {
            ledger: token,
            icpswap_pool: pool,
        }];
        listed.ledger_fees.push(types::pool::LedgerFee {
            ledger: token,
            fee: TokenAmount::from(1_000_u32),
        });
        record_at(PoolStep::SettingsSet { settings: listed }, at(2)).unwrap();
        let to_token = |usdc_out: TokenAmount| WithdrawRequest {
            token_out: token,
            ..withdrawal(usdc_out)
        };
        let plan = |request: &WithdrawRequest| {
            read_state(|state| plan_withdrawal(state, request, None, nobody))
        };
        let fees = TokenAmount::from(TOKEN_PATH_FEES as u64 * FEE);
        assert!(matches!(
            plan(&to_token(fees)),
            Err(PoolError::NotAllowed(why)) if why.contains("fees")
        ));
        let just_above = fees.checked_add(TokenAmount::from(1_u8)).unwrap();
        let ok = plan(&to_token(just_above)).unwrap();
        assert_eq!(ok.path, PayoutPath::Icpswap { pool });
        assert_eq!(
            ok.amount,
            just_above.checked_sub(TokenAmount::from(2 * FEE)).unwrap()
        );
        assert_eq!(ok.ckusdc, ckusdc());
    });
}

/// A balance check while a payout is started and not recorded paid finds no shortfall,
/// whether the ledger has debited it yet or not; a balance below both is one.
#[test]
fn a_check_during_a_payout_finds_no_shortfall() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        let r#ref = register(swap_terms(1, usdc(100), usdc(1), at(1_000)));
        record_at(
            PoolStep::Arrived {
                r#ref,
                credit: credit(1),
                amount: usdc(100),
                block: BlockNumber::new(5),
            },
            at(3),
        )
        .unwrap();
        record_at(
            PoolStep::PayoutStarted {
                r#ref,
                attempt: PayoutAttempt {
                    created_at: at(4),
                    memo: r#ref.into_bytes(),
                    path: PayoutPath::Ckusdc,
                    amount: None,
                    token: None,
                },
            },
            at(4),
        )
        .unwrap();
        let tolerance = TokenAmount::ZERO;
        let debited = usdc(901);
        let check = |observed| read_state(|state| icp_expected(state, observed, tolerance));
        // the transfer landed: 1000 less the user's 99 (the transfer and its fee)
        assert_eq!(check(debited), (debited, false));
        // not yet: a passing surplus
        assert_eq!(check(usdc(1_000)), (debited, false));
        // below both: a shortfall
        assert_eq!(
            check(debited.checked_sub(TokenAmount::ONE).unwrap()),
            (debited, true)
        );
        // before the payout started, the whole pocket is expected
        record_at(
            PoolStep::Paid {
                r#ref,
                token: ckusdc(),
                amount: usdc(99).checked_sub(TokenAmount::from(FEE)).unwrap(),
                block: 1,
                spent: usdc(99),
            },
            at(5),
        )
        .unwrap();
        assert_eq!(check(debited), (debited, false));
    });
}

/// A shortfall latches the ICP latch, the pool's guard refuses on it, and a controller's
/// reopening clears it.
#[test]
fn an_icp_shortfall_closes_the_pool_until_reopened() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        note_icp_check(
            Ok(IcpCheck {
                observed: usdc(900),
                expected: usdc(1_000),
                shortfall: true,
            }),
            at(9),
        );
        assert!(ledger().icp_shortfall_at.is_some());
        base_read(10);
        let config = types::Config {
            pool_chain: Some(ChainId::BASE),
            ..types::Config::default()
        };
        // the Base reads are fresh and sound: the ICP latch alone closes the pool
        assert_eq!(status(&config, 11), Err(PoolClosed::IcpShortfall));
        assert_eq!(
            pool::closed_reason(status(&config, 11), &ledger()),
            Some("the balance check of the ICP pocket found a shortfall".into())
        );
        append_event_at(
            EventType::Books {
                chain_id: ChainId::BASE,
                step: BookStep::PoolPaused { paused: false },
            },
            at(12),
        )
        .unwrap();
        assert_eq!(status(&config, 12), Ok(()));
    });
}

/// A read of the pool chain's balance that passed at `nanos`, kept as the hourly check
/// keeps it.
fn base_read(nanos: u64) {
    crate::storage::timer_state::update(|state| {
        state.books_reads.entry(ChainId::BASE).or_default().read = Some((
            at(nanos),
            BalanceCheck {
                block: BlockNumber::new(nanos),
                observed: TokenAmount::ZERO,
                expected: TokenAmount::ZERO,
            },
        ));
        ((), true)
    });
}

fn status(config: &types::Config, nanos: u64) -> Result<(), PoolClosed> {
    let reads = crate::storage::timer_state::get();
    read_state(|state| {
        crate::books::pool_status(state, &state.pool(), &reads, config, false, at(nanos))
    })
}

/// Review fix: a stake whose pull lands while the check's balance read is out is in the lines
/// read after the read and not in the balance it read. The check expects the lower of the
/// lines before and after: no false shortfall, so nothing latches the pool closed.
#[test]
fn a_stake_landing_during_the_read_is_no_shortfall() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        let tolerance = TokenAmount::ZERO;
        let before = read_state(icp_lines);
        // the balance read sees the pocket before the pull
        let observed = before;
        let id = types::hash::PoolOpId::new([3; 32]);
        record_at(
            PoolStep::StakeStarted {
                id,
                owner: PoolAccount::Icp(account(7)),
                from: account(7),
                amount: usdc(100),
                created_at: at(2),
                move_fee: TokenAmount::ZERO,
            },
            at(2),
        )
        .unwrap();
        let shares = ledger().mint_now(usdc(100)).unwrap();
        record_at(
            PoolStep::StakedIcp {
                id,
                owner: PoolAccount::Icp(account(7)),
                amount: usdc(100),
                move_fee: TokenAmount::ZERO,
                shares,
                block: 9,
            },
            at(3),
        )
        .unwrap();
        let after = read_state(icp_lines);
        assert_eq!(after, usdc(1_100));
        // the lines after alone would cry a shortfall of the stake
        assert_eq!(
            read_state(|state| icp_expected(state, observed, tolerance)),
            (usdc(1_100), true)
        );
        assert_eq!(
            judge(before, after, observed, tolerance),
            (usdc(1_000), false)
        );
        // a real shortfall is still one
        assert_eq!(
            judge(before, after, usdc(999), tolerance),
            (usdc(1_000), true)
        );
    });
}

/// Review fix: a read that fails passes nothing. Once the pool has settings, its guard
/// refuses until a check read the ICP pocket's balance, and again when none did within
/// three intervals, failed reads or not.
#[test]
fn a_failed_read_passes_nothing() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        let interval = types::config::BooksInterval::DEFAULT.0;
        let config = types::Config {
            pool_chain: Some(ChainId::BASE),
            ..types::Config::default()
        };
        base_read(10);
        assert_eq!(status(&config, 11), Err(PoolClosed::IcpNeverChecked));
        note_icp_check(Err(IcpReadFailure::CallFailed), at(12));
        assert_eq!(status(&config, 13), Err(PoolClosed::IcpNeverChecked));
        note_icp_check(
            Ok(IcpCheck {
                observed: usdc(1_000),
                expected: usdc(1_000),
                shortfall: false,
            }),
            at(14),
        );
        assert_eq!(status(&config, 15), Ok(()));
        // three intervals of failed reads later
        let later = 14 + interval.as_nanos() as u64 * 3 + 1;
        base_read(later - 1);
        note_icp_check(Err(IcpReadFailure::CallFailed), at(later - 1));
        assert!(matches!(
            status(&config, later),
            Err(PoolClosed::IcpStale { .. })
        ));
    });
}

/// A payout stopped for a human is still between its start and its payment: the check
/// leaves it out of what it expects, debited or not.
#[test]
fn a_stopped_payout_is_still_pending_for_the_check() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        let r#ref = register(swap_terms(1, usdc(100), usdc(1), at(1_000)));
        record_at(
            PoolStep::Arrived {
                r#ref,
                credit: credit(1),
                amount: usdc(100),
                block: BlockNumber::new(5),
            },
            at(3),
        )
        .unwrap();
        record_at(
            PoolStep::PayoutStarted {
                r#ref,
                attempt: PayoutAttempt {
                    created_at: at(4),
                    memo: r#ref.into_bytes(),
                    path: PayoutPath::Ckusdc,
                    amount: None,
                    token: None,
                },
            },
            at(4),
        )
        .unwrap();
        for _ in 0..types::pool::MAX_PAYOUT_ATTEMPTS {
            record_at(
                PoolStep::AttemptFailed {
                    r#ref,
                    why: "the ledger answered nothing".into(),
                },
                at(5),
            )
            .unwrap();
        }
        assert!(matches!(
            crate::storage::pool::deposit(&r#ref).unwrap().status,
            DepositStatus::PayoutFailed(_)
        ));
        let debited = usdc(901);
        let check = |observed| read_state(|state| icp_expected(state, observed, TokenAmount::ZERO));
        assert_eq!(check(debited), (debited, false));
        assert_eq!(check(usdc(1_000)), (debited, false));
    });
}

/// Review fix (fail-open monitoring): every way the ICP check can fail is a failure of its
/// kind, never a pass: a ledger call that failed or was refused, and an answer that is no
/// balance the lines hold. A failure writes no line and refreshes nothing the guard reads.
#[test]
fn every_failure_of_the_icp_check_is_a_failed_check() {
    let zero = TokenAmount::ZERO;
    let unknown = Err(crate::ic::CallError::Unknown("rejected".into()));
    assert_eq!(
        check_of(unknown, usdc(1), usdc(1), zero),
        Err(IcpReadFailure::CallFailed)
    );
    let refused = Err(crate::ic::CallError::Refused("no".into()));
    assert_eq!(
        check_of(refused, usdc(1), usdc(1), zero),
        Err(IcpReadFailure::CallFailed)
    );
    // wider than the 128 bits the lines hold: no balance, and never read as a huge one
    let too_wide = Nat::from(u128::MAX) + Nat::from(1_u8);
    assert_eq!(
        check_of(Ok(too_wide), usdc(1), usdc(1), zero),
        Err(IcpReadFailure::BadAnswer)
    );
    // a balance read is a check, judged on the lower of the lines
    assert_eq!(
        check_of(Ok(Nat::from(usdc(1))), usdc(1), usdc(2), zero),
        Ok(IcpCheck {
            observed: usdc(1),
            expected: usdc(1),
            shortfall: false,
        })
    );
    on_fresh_memory(move || {
        crate::storage::init();
        funded_pool(usdc(1_000));
        let count = crate::storage::events::event_count();
        for failure in [IcpReadFailure::CallFailed, IcpReadFailure::BadAnswer] {
            note_icp_check(Err(failure), at(5));
            let read = crate::storage::timer_state::get().icp_read;
            assert_eq!(read.read_at(), None);
            assert_eq!(read.failed_at, Some(at(5)));
        }
        assert_eq!(crate::storage::events::event_count(), count, "no line");
    });
}

/// A read that failed and a shortfall each log a line; a read that passed logs none.
#[test]
fn a_failed_read_and_a_shortfall_each_log_a_line() {
    let zero = TokenAmount::ZERO;
    let failed = check_of(
        Err(crate::ic::CallError::Unknown("rejected".into())),
        zero,
        zero,
        zero,
    );
    assert!(check_log(&failed).is_some_and(|line| line.contains("CallFailed")));
    let short = check_of(Ok(Nat::from(1_u8)), usdc(5), usdc(5), zero);
    assert!(check_log(&short).is_some_and(|line| line.starts_with("ALERT")));
    let fine = check_of(Ok(Nat::from(usdc(5))), usdc(5), usdc(5), zero);
    assert_eq!(check_log(&fine), None);
}

/// An ICP read with no news is kept beside the timers and writes no line; a surplus other
/// than the last one recorded, or a shortfall, is a line.
#[test]
fn an_icp_read_with_no_news_writes_no_line() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        let count = crate::storage::events::event_count;
        let start = count();
        let check = |observed: u64, shortfall: bool| IcpCheck {
            observed: usdc(observed),
            expected: usdc(1_000),
            shortfall,
        };
        note_icp_check(Ok(check(1_000, false)), at(1));
        assert_eq!(count(), start);
        assert_eq!(
            crate::storage::timer_state::get().icp_read.read_at(),
            Some(at(1))
        );
        note_icp_check(Ok(check(1_050, false)), at(2));
        assert_eq!(count(), start + 1, "a new surplus");
        note_icp_check(Ok(check(1_050, false)), at(3));
        assert_eq!(count(), start + 1, "the same surplus again");
        note_icp_check(Ok(check(900, true)), at(4));
        assert_eq!(count(), start + 2, "a shortfall");
        assert!(ledger().icp_shortfall_at.is_some());
    });
}

/// L-01: before the pool has settings the ICP check records nothing: no failed check lands in
/// the permanent log every books interval of a canister that has not set the pool up.
#[test]
fn a_check_before_settings_records_nothing() {
    use std::future::Future;
    on_fresh_memory(|| {
        crate::storage::init();
        let count = crate::storage::events::event_count();
        let mut check = std::pin::pin!(check_icp_pocket(TokenAmount::ZERO));
        let context = &mut std::task::Context::from_waker(std::task::Waker::noop());
        assert!(
            check.as_mut().poll(context).is_ready(),
            "no ledger is asked"
        );
        assert_eq!(crate::storage::events::event_count(), count);
    });
}

/// The doors' wire reading, which a consent message shares: a 32-byte subaccount, amounts of
/// at most 128 bits, the owner parsed; anything else refused as the door refuses it.
#[test]
fn a_stake_and_a_withdrawal_are_read_off_the_wire_as_their_doors_read_them() {
    use settlement_api::types::pool::{IcpAccount as WireAccount, PoolAccount as WirePool};
    let caller = account(7).owner;
    let evm_owner: types::EvmAddress = "0x1111111111111111111111111111111111111111"
        .parse()
        .unwrap();
    let stake = StakeRequest::of(
        caller,
        StakeIcpArgs {
            from_subaccount: Some(vec![7; 32]),
            amount: Nat::from(250_000_000_u64),
            owner: WirePool::Evm("0x1111111111111111111111111111111111111111".to_string()),
            move_fee: Nat::from(0_u8),
        },
    );
    assert_eq!(
        stake,
        Ok(StakeRequest {
            caller,
            from_subaccount: Some([7; 32]),
            amount: TokenAmount::from(250_000_000_u64),
            owner: PoolAccount::Evm(evm_owner),
            move_fee: TokenAmount::ZERO,
        })
    );
    let short = StakeRequest::of(
        caller,
        StakeIcpArgs {
            from_subaccount: Some(vec![7; 31]),
            amount: Nat::from(1_u8),
            owner: WirePool::Icp(WireAccount {
                owner: caller,
                subaccount: None,
            }),
            move_fee: Nat::from(0_u8),
        },
    );
    assert_eq!(
        short,
        Err(PoolError::NotAllowed(
            "from_subaccount must be 32 bytes, not 31".to_string()
        ))
    );
    let wire = |usdc: Nat| WithdrawIcpArgs {
        from: None,
        usdc,
        max_shares: Nat::from(10_u8),
        token_out: caller,
        min_out: Nat::from(99_990_000_u64),
        to: WireAccount {
            owner: caller,
            subaccount: None,
        },
        move_fee: Nat::from(0_u8),
    };
    assert_eq!(
        WithdrawRequest::of(caller, wire(Nat::from(100_000_000_u64))),
        Ok(WithdrawRequest {
            caller,
            from: None,
            usdc: TokenAmount::from(100_000_000_u64),
            max_shares: Shares::new(10),
            token_out: caller,
            min_out: 99_990_000,
            to: IcpAccount {
                owner: caller,
                subaccount: None
            },
            move_fee: TokenAmount::ZERO,
        })
    );
    assert_eq!(
        WithdrawRequest::of(caller, wire(Nat::from(u128::MAX) + Nat::from(1_u8))),
        Err(PoolError::NotAllowed(
            "usdc does not fit 128 bits".to_string()
        ))
    );
}

/// Records the pool's settings with the least move fees a controller set.
fn with_least_move_fees(base_to_icp: u64, icp_to_base: u64) {
    let mut settings = ledger().settings.unwrap();
    settings.least_move_fee = types::pool::MoveFees {
        base_to_icp: TokenAmount::from(base_to_icp),
        icp_to_base: TokenAmount::from(icp_to_base),
    };
    record_at(PoolStep::SettingsSet { settings }, at(3)).unwrap();
}

/// An ICP stake pays at least the least fee of an ICP to Base move, and an ICP withdrawal
/// at least that of a Base to ICP move, each below its amount; none while they are zero.
#[test]
fn the_icp_doors_hold_the_move_fee_to_its_least() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        let stake = |move_fee: u64| {
            let request = StakeRequest {
                move_fee: TokenAmount::from(move_fee),
                ..stake_request(usdc(100))
            };
            read_state(|state| check_stake(state, &request, nobody))
        };
        let withdraw = |move_fee: u64| {
            let request = WithdrawRequest {
                move_fee: TokenAmount::from(move_fee),
                ..withdrawal(usdc(100))
            };
            read_state(|state| plan_withdrawal(state, &request, None, nobody)).map(drop)
        };
        assert_eq!(stake(0), Ok(()));
        assert_eq!(withdraw(0), Ok(()));
        with_least_move_fees(1_500_000, 1_400_000);
        assert!(
            matches!(stake(1_399_999), Err(PoolError::NotAllowed(why)) if why.contains("1400000"))
        );
        assert_eq!(stake(1_400_000), Ok(()));
        assert!(
            matches!(withdraw(1_499_999), Err(PoolError::NotAllowed(why)) if why.contains("1500000"))
        );
        assert_eq!(withdraw(1_500_000), Ok(()));
        assert!(
            matches!(stake(100_000_000), Err(PoolError::NotAllowed(why)) if why.contains("below"))
        );
    });
}
