use super::*;
use crate::pool::PoolSettings;

fn usdc(micro: u64) -> TokenAmount {
    TokenAmount::from(micro)
}

fn settings(premium_bps: u16) -> PoolSettings {
    PoolSettings {
        premium_bps,
        ..PoolSettings::default()
    }
}

/// A sale that makes exactly the expected ckUSDC pays the plan's amount at least, whatever
/// the premium, the fee and the amount.
#[test]
fn the_expected_sale_pays_at_least_the_plan() {
    for planned in [123_u64, 1_000_000, 7_777_777, 100_000_000, 999_999_999] {
        for fee in [0_u64, 1, 50_000] {
            for bps in [0_u16, 1, 10, 999, 1_000] {
                let settings = settings(bps);
                let expected = expected_received(&settings, usdc(planned), usdc(fee)).unwrap();
                let paid = payout_of(&settings, expected, usdc(fee)).unwrap();
                assert!(paid >= usdc(planned), "{planned} {fee} {bps}: {paid}");
            }
        }
    }
    // 100 USDC, 5 cents, 10 bps: exactly the plan
    let settings = settings(10);
    let expected = expected_received(&settings, usdc(100_000_000), usdc(50_000)).unwrap();
    assert_eq!(expected, usdc(100_150_151));
    assert_eq!(
        payout_of(&settings, expected, usdc(50_000)),
        Some(usdc(100_000_000))
    );
}

/// Half the slippage, rounded up: never below the exact half.
#[test]
fn the_sale_floor_is_half_the_slippage_rounded_up() {
    assert_eq!(sale_floor(usdc(100_150_151), 100), Some(usdc(99_649_401)));
    assert_eq!(sale_floor(usdc(1_000), 0), Some(usdc(1_000)));
    // 45 bps: 22.5 bps of 1,000,000 is 2,250
    assert_eq!(sale_floor(usdc(1_000_000), 45), Some(usdc(997_750)));
    // 1 bps of 3 rounds up to all of it
    assert_eq!(sale_floor(usdc(3), 1), Some(usdc(3)));
    // a sale at its floor pays at most 2.5% under the plan
    let settings = settings(10);
    for planned in [1_000_000_u64, 100_000_000, 999_999_999] {
        let expected = expected_received(&settings, usdc(planned), usdc(50_000)).unwrap();
        let floor = sale_floor(expected, MAX_SLIPPAGE_BPS).unwrap();
        assert!(fits(
            usdc(planned),
            payout_of(&settings, floor, usdc(50_000)).unwrap()
        ));
    }
}

/// The premium is the settings' share of what the sale made, rounded up: toward the pool.
#[test]
fn the_premium_comes_from_what_the_sale_made_rounded_toward_the_pool() {
    let settings = settings(10);
    // 10 bps of 99,649,401 is 99,649.401: the pool keeps 99,650
    assert_eq!(
        payout_of(&settings, usdc(99_649_401), usdc(50_000)),
        Some(usdc(99_649_401 - 99_650 - 50_000))
    );
    // the fees take all of it
    assert_eq!(payout_of(&settings, usdc(50_050), usdc(50_000)), None);
}

#[test]
fn the_window_holds_five_percent_both_ways_and_not_a_unit_more() {
    let planned = usdc(100_000_000);
    assert!(fits(planned, planned));
    assert!(fits(planned, usdc(105_000_000)));
    assert!(fits(planned, usdc(95_000_000)));
    assert!(!fits(planned, usdc(105_000_001)));
    assert!(!fits(planned, usdc(94_999_999)));
    // nothing fits a plan of nothing but nothing
    assert!(fits(TokenAmount::ZERO, TokenAmount::ZERO));
    assert!(!fits(TokenAmount::ZERO, usdc(1)));
}

#[test]
fn the_base_pocket_must_hold_the_plan_and_five_percent_at_open() {
    assert_eq!(open_cover(usdc(100_000_000)), Some(usdc(105_000_000)));
    assert_eq!(open_cover(usdc(1)), Some(usdc(2)));
    assert_eq!(open_cover(usdc(20)), Some(usdc(21)));
}

#[test]
fn an_amount_too_large_is_refused_never_wrapped() {
    let settings = settings(10);
    assert_eq!(
        expected_received(&settings, TokenAmount::MAX, usdc(1)),
        None
    );
    assert_eq!(sale_floor(TokenAmount::MAX, 100), None);
    assert_eq!(open_cover(TokenAmount::MAX), None);
    assert!(!fits(TokenAmount::MAX, TokenAmount::ZERO));
    assert_eq!(sale_floor(usdc(1), 20_001), None);
}

#[test]
fn the_first_minimum_is_the_first_spending_steps_or_the_payouts() {
    use crate::swapic::{Order, Payout, Step};
    use alloy_primitives::{Address, Bytes, FixedBytes, U256};
    let usdc = Address::repeat_byte(0x83);
    let step = |token: Address, min: u64| Step {
        target: Address::repeat_byte(0x61),
        data: Bytes::new(),
        token,
        min: U256::from(min),
        writes: vec![],
    };
    let mut order = Order {
        r#ref: FixedBytes([1; 32]),
        payer: Address::repeat_byte(0x36),
        tokenIn: usdc,
        amountIn: U256::from(100_u64),
        deadline: U256::from(1_u64),
        steps: vec![step(Address::ZERO, 5), step(usdc, 97), step(usdc, 1)],
        payout: Payout {
            token: Address::repeat_byte(0x42),
            recipient: Address::repeat_byte(0x11),
            minOut: U256::from(3_u64),
        },
        memo: Default::default(),
    };
    assert_eq!(first_minimum(&order), usdc_micro(97));
    // no step: the payout's, when it pays the token in
    order.steps.clear();
    assert_eq!(first_minimum(&order), TokenAmount::ZERO);
    order.payout.token = usdc;
    assert_eq!(first_minimum(&order), usdc_micro(3));
}

fn usdc_micro(micro: u64) -> TokenAmount {
    TokenAmount::from(micro)
}
