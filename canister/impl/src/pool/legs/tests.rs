use super::*;

#[test]
fn the_first_leg_buys_what_the_second_needs() {
    // the second leg quotes 2,000 for 1,000 ICP sold: 1,500 needs 750 of it, plus the fees
    assert_eq!(
        first_leg_least(
            TokenAmount::from(1_000_u32),
            TokenAmount::from(2_000_u32),
            TokenAmount::from(1_500_u32),
            TokenAmount::from(30_u32),
        ),
        Some(TokenAmount::from(780_u32))
    );
    // rounded up
    assert_eq!(
        first_leg_least(
            TokenAmount::from(1_000_u32),
            TokenAmount::from(3_000_u32),
            TokenAmount::from(1_000_u32),
            TokenAmount::ZERO,
        ),
        Some(TokenAmount::from(334_u32))
    );
    assert_eq!(
        first_leg_least(
            TokenAmount::from(1_000_u32),
            TokenAmount::ZERO,
            TokenAmount::from(1_000_u32),
            TokenAmount::ZERO,
        ),
        None
    );
}

#[test]
fn a_legs_cost_is_its_approvals_and_its_deposit_or_its_undo() {
    let fee = TokenAmount::from(10_u32);
    let mut leg = SwapLeg {
        approvals: Some(2),
        ..SwapLeg::default()
    };
    assert_eq!(cost_in(&leg, fee), Some(TokenAmount::from(20_u32)));
    leg.begun = true;
    leg.amount_in = Some(TokenAmount::from(1_000_u32));
    assert_eq!(cost_in(&leg, fee), Some(TokenAmount::from(1_030_u32)));
    leg.undone = Some(TokenAmount::from(20_u32));
    assert_eq!(cost_in(&leg, fee), Some(TokenAmount::from(40_u32)));
}

#[test]
fn an_output_paid_on_takes_two_fees_and_one_kept_takes_one() {
    let spec = LegSpec {
        id: PoolRef::new([1; 32]),
        leg: None,
        pool: Principal::from_slice(&[2; 10]),
        token_in: Principal::from_slice(&[3; 10]),
        token_out: Principal::from_slice(&[4; 10]),
        amount_in: TokenAmount::from(100_u32),
        least: TokenAmount::ONE,
        fee_in: TokenAmount::from(1_u32),
        fee_out: TokenAmount::from(5_u32),
        pays_out: true,
    };
    assert_eq!(
        spec.send_of(TokenAmount::from(50_u32)),
        TokenAmount::from(40_u32)
    );
    let kept = LegSpec {
        pays_out: false,
        ..spec
    };
    assert_eq!(
        kept.send_of(TokenAmount::from(50_u32)),
        TokenAmount::from(45_u32)
    );
}

/// P11e: an approval given before a run whose swap never began is one fee more, which the
/// user pays out of what the leg deposits: the leg costs the canister's account what its path
/// budgeted (one approval, the deposit and its pull's fee), however many approvals it took.
#[test]
fn a_retried_approval_is_paid_out_of_what_the_leg_deposits() {
    let fee = TokenAmount::from(10_u32);
    let amount = TokenAmount::from(1_000_u32);
    assert_eq!(after_approvals(amount, fee, None), Some(amount));
    assert_eq!(after_approvals(amount, fee, Some(0)), Some(amount));
    // one approval given before this run: the run's own is the second
    let deposited = after_approvals(amount, fee, Some(1)).unwrap();
    assert_eq!(deposited, TokenAmount::from(990_u32));
    let leg = SwapLeg {
        approvals: Some(2),
        begun: true,
        amount_in: Some(deposited),
        ..SwapLeg::default()
    };
    let budget = SwapLeg {
        approvals: Some(1),
        begun: true,
        amount_in: Some(amount),
        ..SwapLeg::default()
    };
    assert_eq!(cost_in(&leg, fee), cost_in(&budget, fee));
    // approvals that take all of it leave nothing to deposit
    assert_eq!(after_approvals(amount, fee, Some(100)), None);
}

/// Two pools as a fake quotes them: whether each trades its pair, and what each quotes.
struct Quoting {
    trades: (bool, bool),
    quotes: (u32, u32),
}

impl Ledgers for Quoting {
    async fn transfer(
        &self,
        _: Principal,
        _: types::pool::IcpAccount,
        _: TokenAmount,
        _: crate::ic::Dedup,
    ) -> Result<u64, CallError> {
        unreachable!("a chained quote moves nothing")
    }
    async fn pull(
        &self,
        _: Principal,
        _: types::pool::IcpAccount,
        _: TokenAmount,
        _: crate::ic::Dedup,
    ) -> Result<u64, CallError> {
        unreachable!("a chained quote moves nothing")
    }
    async fn approve(&self, _: Principal, _: Principal, _: TokenAmount) -> Result<(), CallError> {
        unreachable!("a chained quote moves nothing")
    }
    async fn direction(
        &self,
        pool: Principal,
        _: Principal,
        _: Principal,
    ) -> Result<Option<bool>, CallError> {
        let trades = if pool == first() {
            self.trades.0
        } else {
            self.trades.1
        };
        Ok(trades.then_some(true))
    }
    async fn quote(
        &self,
        pool: Principal,
        _: TokenAmount,
        _: bool,
    ) -> Result<TokenAmount, CallError> {
        let quote = if pool == first() {
            self.quotes.0
        } else {
            self.quotes.1
        };
        Ok(TokenAmount::from(quote))
    }
    async fn swap(
        &self,
        _: Principal,
        _: TokenAmount,
        _: bool,
        _: TokenAmount,
        _: TokenAmount,
        _: TokenAmount,
    ) -> Result<TokenAmount, CallError> {
        unreachable!("a chained quote moves nothing")
    }
    /// Read in the first pool's round, beside its quote: what the pool holds of the canister's.
    async fn unused(&self, pool: Principal) -> Result<(TokenAmount, TokenAmount), CallError> {
        assert_eq!(pool, first(), "the round is the first pool's alone");
        Ok((TokenAmount::from(7_u32), TokenAmount::from(9_u32)))
    }
    async fn withdraw(
        &self,
        _: Principal,
        _: Principal,
        _: TokenAmount,
        _: TokenAmount,
    ) -> Result<(), CallError> {
        unreachable!("a chained quote moves nothing")
    }
}

fn first() -> Principal {
    Principal::from_slice(&[1; 10])
}

/// The two chained quotes' refusals read as each payout's lines always named them: an ICP
/// swap's (`min_usdc needs`) and a payout's through ICP (`needed`).
#[test]
fn a_chained_quote_names_its_pools_and_need_as_its_caller_says() {
    let chain = |ledgers: &Quoting, names: ChainNames| {
        crate::test_util::block_on(chained_least(
            ledgers,
            (
                first(),
                Principal::from_slice(&[3; 10]),
                TokenAmount::from(1_000_u32),
            ),
            (
                Principal::from_slice(&[2; 10]),
                Principal::from_slice(&[4; 10]),
                Principal::from_slice(&[5; 10]),
            ),
            TokenAmount::from(30_u32),
            TokenAmount::from(1_500_u32),
            names,
        ))
        .unwrap()
        .map(|chained| chained.least)
    };
    let swap = || ChainNames {
        first: "the first pool",
        second: "the second pool",
        need: "min_usdc needs",
    };
    let payout = || ChainNames {
        first: "the ICP/ckUSDC pool",
        second: "the token's ICP pool",
        need: "needed",
    };
    let quoting = |trades, quotes| Quoting { trades, quotes };
    assert_eq!(
        chain(&quoting((false, true), (1_030, 2_000)), swap()),
        Err("the first pool does not trade the pair".to_string())
    );
    assert_eq!(
        chain(&quoting((true, false), (1_030, 2_000)), swap()),
        Err("the second pool does not trade the pair".to_string())
    );
    assert_eq!(
        chain(&quoting((true, true), (1_030, 1_499)), swap()),
        Err("the pools quote 1499 through ICP, below the 1500 min_usdc needs".to_string())
    );
    assert_eq!(
        chain(&quoting((false, true), (1_030, 2_000)), payout()),
        Err("the ICP/ckUSDC pool does not trade the pair".to_string())
    );
    assert_eq!(
        chain(&quoting((true, false), (1_030, 2_000)), payout()),
        Err("the token's ICP pool does not trade the pair".to_string())
    );
    assert_eq!(
        chain(&quoting((true, true), (1_030, 1_499)), payout()),
        Err("the pools quote 1499 through ICP, below the 1500 needed".to_string())
    );
    // 1,000 sold of the 1,030 bought quotes 2,000: 1,500 needs 750 of it, plus the fees
    assert_eq!(
        chain(&quoting((true, true), (1_030, 2_000)), payout()),
        Ok(TokenAmount::from(780_u32))
    );
}

/// The first pool's quote is read in the first leg's own round, beside what the pool holds of
/// the canister's: the chained quote hands the leg that round for the amount it quoted, so the
/// leg asks the first pool nothing again.
#[test]
fn a_chained_quote_hands_the_first_leg_its_round() {
    let quoting = Quoting {
        trades: (true, true),
        quotes: (1_030, 2_000),
    };
    let chained = crate::test_util::block_on(chained_least(
        &quoting,
        (
            first(),
            Principal::from_slice(&[3; 10]),
            TokenAmount::from(1_000_u32),
        ),
        (
            Principal::from_slice(&[2; 10]),
            Principal::from_slice(&[4; 10]),
            Principal::from_slice(&[5; 10]),
        ),
        TokenAmount::from(30_u32),
        TokenAmount::from(1_500_u32),
        ChainNames {
            first: "the first pool",
            second: "the second pool",
            need: "needed",
        },
    ))
    .unwrap()
    .unwrap();
    assert_eq!(
        chained.read,
        RoundRead {
            amount_in: TokenAmount::from(1_000_u32),
            round: Round {
                quoted: TokenAmount::from(1_030_u32),
                // the first pool trades zero for one here: ckUSDC is its token0
                before: PoolHeld {
                    ckusdc: TokenAmount::from(7_u32),
                    token: TokenAmount::from(9_u32),
                },
                // no approval of this token may stand: none is read
                allowed: None,
            },
        }
    );
}
