//! On the REAL scaler: a KyberSwap USDC to WETH route on Base built for 500 USDC (scalable
//! sources only, the deployed Runner as its receiver) and KyberSwap's InputScalingHelper's own
//! answers for it, read on Base at block 52,988,860 (`samples/scaler_base.json`, captured
//! read-only for lane C1, 2026-10-23): 501.65 and 475.000001 rewritten, 530 refused.

use super::*;
use alloy_primitives::U256;
use alloy_primitives::{Address, FixedBytes};
use serde_json::Value;
use types::swapic::{addr, swapCall, Payout, KYBER_ROUTER};

fn sample() -> Value {
    serde_json::from_str(include_str!("samples/scaler_base.json")).unwrap()
}

fn hex_of(value: &Value) -> Vec<u8> {
    hex::decode(value.as_str().unwrap().trim_start_matches("0x")).unwrap()
}

fn route() -> Vec<u8> {
    hex_of(&sample()["route_calldata"])
}

/// The scaler's raw answer for `amount`.
fn answer(amount: u64) -> Vec<u8> {
    let sample = sample();
    let found = sample["answers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|answer| {
            answer["new_amount"]
                .as_str()
                .and_then(|text| text.parse().ok())
                == Some(amount)
        })
        .unwrap()
        .clone();
    hex_of(&found["result"])
}

fn usdc() -> Address {
    "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"
        .parse()
        .unwrap()
}

fn swap_step(route: Vec<u8>, min: u64) -> Step {
    Step {
        target: addr(KYBER_ROUTER),
        data: route.into(),
        token: usdc(),
        min: U256::from(min),
        writes: vec![],
    }
}

fn amount(micro: u64) -> TokenAmount {
    TokenAmount::from(micro)
}

/// The plan: the real route as its one step, 500 USDC in, WETH out, its deadline `deadline`.
fn plan(min: u64, deadline: u64) -> PinnedOrder {
    let order = Order {
        r#ref: FixedBytes([0x61; 32]),
        payer: Address::repeat_byte(0x36),
        tokenIn: usdc(),
        amountIn: U256::from(500_000_000_u64),
        deadline: U256::from(deadline),
        steps: vec![swap_step(route(), min)],
        payout: Payout {
            token: "0x4200000000000000000000000000000000000006"
                .parse()
                .unwrap(),
            recipient: Address::repeat_byte(0x11),
            minOut: U256::from(200_000_000_000_000_000_u64),
        },
    };
    PinnedOrder {
        order_id: [0x62; 32],
        order: swapic::encode_order(&order),
        gas_limit: 900_000,
        deadline_s: deadline,
    }
}

/// The lead's prerequisite P: the real answer for 501.65 USDC passes the rewritten step's
/// checks, which hold to exactly what the scaler does: all of the new amount sold, the old
/// minimum scaled down to it, the same swap otherwise.
#[test]
fn the_real_scaler_answer_passes_the_rewritten_step_checks() {
    let step = swap_step(route(), 0);
    let before = KyberSwap::of_step(&step).unwrap().unwrap();
    assert_eq!(before.amount, U256::from(500_000_000_u64));
    assert_eq!(before.min_return, U256::from(206_702_236_984_178_741_u64));
    for (new, least) in [
        (501_650_000_u64, 207_384_354_366_226_530_u64),
        (475_000_001, 196_367_125_548_374_277),
    ] {
        let rewritten = scaled_of(&answer(new)).unwrap();
        let step = rescaled(&step, rewritten, amount(new)).unwrap();
        let after = KyberSwap::of_step(&step).unwrap().unwrap();
        assert_eq!(after.amount, U256::from(new));
        assert_eq!(after.min_return, U256::from(least), "{new}");
        assert_eq!(after.dst_receiver, before.dst_receiver);
    }
}

/// 530 USDC is past what the route can scale to: the scaler refuses it, and an answer that is
/// not the scaler's return is no route either.
#[test]
fn a_scaler_refusal_or_a_bad_answer_is_no_route() {
    assert_eq!(
        scaled_of(&answer(530_000_000)),
        Err("the scaler refused the amount".to_string())
    );
    assert!(scaled_of(&[0x01; 31]).is_err());
}

/// A rewritten route that is another swap, sells another amount, or guarantees less than
/// the old minimum scaled is refused.
#[test]
fn a_rescaled_swap_that_moves_its_receiver_or_amount_is_refused() {
    use alloy_sol_types::SolCall;
    let step = swap_step(route(), 0);
    let real = scaled_of(&answer(501_650_000)).unwrap();
    // the answer for another amount
    let other = scaled_of(&answer(475_000_001)).unwrap();
    assert!(rescaled(&step, other, amount(501_650_000)).is_err());
    let tampered = |change: &dyn Fn(&mut swapCall)| {
        let mut call = swapCall::abi_decode(&real).unwrap();
        change(&mut call);
        rescaled(&step, call.abi_encode(), amount(501_650_000))
    };
    assert!(tampered(&|_| {}).is_ok());
    assert!(matches!(
        tampered(&|call| call.execution.desc.dstReceiver = Address::repeat_byte(0xbd)),
        Err(why) if why.contains("another swap")
    ));
    assert!(tampered(&|call| call.execution.desc.dstToken = usdc()).is_err());
    assert!(tampered(&|call| call.execution.callTarget = Address::repeat_byte(0xbe)).is_err());
    assert!(matches!(
        tampered(&|call| call.execution.desc.minReturnAmount -= U256::from(1)),
        Err(why) if why.contains("below")
    ));
    assert!(tampered(&|call| call.execution.desc.amount += U256::from(1)).is_err());
    // a route that is no KyberSwap swap at all
    assert!(rescaled(&step, vec![0xde, 0xad], amount(501_650_000)).is_err());
}

/// What a plan says before any read: within the window, its deadline ahead, its first step
/// within the payout, one swap at most; the order then spends the payout's exact amount.
#[test]
fn a_plan_is_decided_before_any_read() {
    let planned = amount(500_000_000);
    let (order, swap) = decide(&plan(0, 1_000), planned, amount(501_650_000), 100).unwrap();
    assert_eq!(order.amountIn, U256::from(501_650_000_u64));
    assert_eq!(swap, Some(0));
    // more than 5% from the plan, either way: waits
    for far in [474_999_999_u64, 525_000_001] {
        assert!(matches!(
            decide(&plan(0, 1_000), planned, amount(far), 100),
            Err(Fit::Wait(_))
        ));
    }
    // its order's deadline under a minute away: waits
    assert!(matches!(
        decide(&plan(0, 1_000), planned, amount(500_000_000), 941),
        Err(Fit::Wait(_))
    ));
    assert!(decide(&plan(0, 1_000), planned, amount(500_000_000), 940).is_ok());
    // a first step asking more than the payout could only revert: undone
    assert!(matches!(
        decide(&plan(490_000_001, 1_000), planned, amount(490_000_000), 100),
        Err(Fit::Undo(_))
    ));
    // a plan that swaps twice: waits
    let mut twice = swapic::decode_order(&plan(0, 1_000).order).unwrap();
    twice.steps.push(twice.steps[0].clone());
    let twice = PinnedOrder {
        order: swapic::encode_order(&twice),
        ..plan(0, 1_000)
    };
    assert!(matches!(
        decide(&twice, planned, amount(500_000_000), 100),
        Err(Fit::Wait(_))
    ));
}

#[test]
fn a_scaler_answer_is_read_up_to_its_route_and_no_more() {
    assert_eq!(scaled_cap(&[0; 3_812]), 2 * (3_812 + 160) + 1_024);
    assert_eq!(scaled_cap(&[0; 40_000]), MAX_SCALED_BYTES);
    // the real answer fits its cap
    let answer = answer(501_650_000);
    assert!(2 * answer.len() as u64 + 256 < scaled_cap(&route()));
}

/// The review (2026-10-23): a first step asking more than the payout could only revert, so
/// it undoes the payout at once, even when the payout is also outside the window.
#[test]
fn a_first_minimum_above_the_payout_undoes_even_outside_the_window() {
    let planned = amount(500_000_000);
    assert!(matches!(
        decide(&plan(490_000_001, 1_000), planned, amount(470_000_000), 100),
        Err(Fit::Undo(_))
    ));
}
