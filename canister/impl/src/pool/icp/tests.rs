use super::*;
use crate::storage::events::append_event_at;
use crate::storage::on_fresh_memory;
use crate::storage::pool::tests::{
    account, at, ckusdc, credit, funded_pool, register, swap_terms, usdc, FEE,
};
use crate::storage::pool::{ledger, record_at};
use types::books::{BalanceCheck, BookStep, PoolClosed};
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
        let (burned, path) = plan(&withdrawal(usdc(400)), None).unwrap();
        assert_eq!(path, PayoutPath::Ckusdc);
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
        // a swap holds 99 of the ICP pocket: 902 is more than its free ckUSDC
        register(swap_terms(1, usdc(100), usdc(1), at(1_000)));
        assert_eq!(
            plan(&withdrawal(usdc(902)), None),
            Err(PoolError::Closed(POCKET_SHORT.into()))
        );
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
                },
            },
            at(4),
        )
        .unwrap();
        let tolerance = TokenAmount::ZERO;
        let debited = usdc(901).checked_sub(TokenAmount::from(FEE)).unwrap();
        let check = |observed| read_state(|state| icp_expected(state, observed, tolerance));
        // the transfer landed: 1000 less 99 and the fee
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
                amount: usdc(99),
                block: 1,
                spent: usdc(99).checked_add(TokenAmount::from(FEE)).unwrap(),
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
        record_at(
            PoolStep::IcpChecked {
                check: IcpCheck {
                    observed: usdc(900),
                    expected: usdc(1_000),
                    shortfall: true,
                },
            },
            at(9),
        )
        .unwrap();
        assert!(ledger().icp_shortfall_at.is_some());
        let books = |step: BookStep, nanos: u64| {
            append_event_at(
                EventType::Books {
                    chain_id: ChainId::BASE,
                    step,
                },
                at(nanos),
            )
            .unwrap();
        };
        books(
            BookStep::Checked {
                check: BalanceCheck {
                    block: BlockNumber::new(1),
                    observed: TokenAmount::ZERO,
                    expected: TokenAmount::ZERO,
                },
                shortfall: false,
            },
            10,
        );
        let config = types::Config {
            pool_chain: Some(ChainId::BASE),
            ..types::Config::default()
        };
        let status =
            || read_state(|state| crate::books::pool_status(state, &config, false, at(11)));
        // the Base books are fresh and sound: the ICP latch alone closes the pool
        assert_eq!(status(), Err(PoolClosed::IcpShortfall));
        assert_eq!(
            pool::closed_reason(status(), &ledger()),
            Some("the balance check of the ICP pocket found a shortfall".into())
        );
        books(BookStep::PoolPaused { paused: false }, 12);
        assert_eq!(status(), Ok(()));
    });
}
