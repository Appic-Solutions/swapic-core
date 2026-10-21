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
