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
        memo: Default::default(),
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
    let decided = decide(&plan(0, 1_000), planned, amount(501_650_000), 100).unwrap();
    assert_eq!(decided.order.amountIn, U256::from(501_650_000_u64));
    assert_eq!((decided.swap, decided.across), (Some(0), None));
    assert_eq!(decided.amount, amount(501_650_000));
    assert_eq!(decided.gas_limit, plan(0, 1_000).gas_limit);
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

/// T0's real Across step (3 USDC from Base, the identity deposit, filled on Arbitrum).
fn across_plan(deadline: u64) -> PinnedOrder {
    let t0: Value = serde_json::from_str(include_str!(
        "../../../../types/src/swapic/across/samples/t0.json"
    ))
    .unwrap();
    let run = hex_of(&t0["run_data"]);
    let mut order = swapic::decode_order(&run[4..]).unwrap();
    order.deadline = U256::from(deadline);
    PinnedOrder {
        order_id: [0x63; 32],
        order: swapic::encode_order(&order),
        gas_limit: 900_000,
        deadline_s: deadline,
    }
}

/// What the fit hands the payout checks: the order it fitted.
struct Fitter(std::cell::RefCell<Option<Order>>);

impl PlanReads for Fitter {
    async fn scaled(&self, _route: &[u8], _amount: TokenAmount) -> Result<Vec<u8>, String> {
        Err("an Across plan reads no scaler".to_string())
    }

    fn checked(
        &self,
        _payout: &EvmPayout,
        order: &Order,
        order_bytes: &[u8],
        gas_limit: u64,
        _now: Timestamp,
    ) -> Result<PinnedOrder, PoolError> {
        *self.0.borrow_mut() = Some(order.clone());
        Ok(PinnedOrder {
            order_id: [0; 32],
            order: order_bytes.to_vec(),
            gas_limit,
            deadline_s: 0,
        })
    }
}

/// BSC design 7.5: an Across plan asks nothing first (the fit writes the amount into its
/// least), fits within the window either way, and is rewritten with no read: M and the step's
/// minimum the payout's exact amount, F the same ratio of it, every other byte as it was.
#[test]
fn an_across_plan_is_fitted_with_no_read_to_the_exact_amount() {
    let planned = amount(3_000_000);
    let plan = across_plan(1_000);
    let order = swapic::decode_order(&plan.order).unwrap();
    assert_eq!(types::pool::plan::first_minimum(&order), TokenAmount::ZERO);
    // 4% below the plan still fits, and is never undone for its least
    let decided = decide(&plan, planned, amount(2_880_000), 100).unwrap();
    // its Across step's place, and no swap: nothing is read ahead for it
    assert_eq!((decided.swap, decided.across), (None, Some(0)));
    let decided = decided.order;
    let planned_order = order.clone();
    crate::storage::on_fresh_memory(move || {
        let order = planned_order;
        let fitter = Fitter(Default::default());
        crate::storage::init();
        // the fit reads the payout as it stands: one waiting with this plan
        let payout = crate::storage::pool::tests::waiting_withdrawal(
            "0x1111111111111111111111111111111111111111".parse().unwrap(),
        );
        let fitted = crate::test_util::block_on(fit(
            &fitter,
            payout,
            Decided {
                order: decided,
                swap: None,
                across: Some(0),
                amount: amount(2_880_000),
                gas_limit: 900_000,
            },
            Timestamp::from_secs(100).unwrap(),
        ))
        .unwrap();
        let before = types::swapic::across::fit_step(&order.steps[0], U256::from(3_000_000u32)).unwrap();
        assert_eq!(before, order.steps[0], "fitting the planned amount changes nothing");
        let step = &fitted.order.steps[0];
        assert_eq!(step.min, U256::from(2_880_000u32));
        assert_eq!(fitted.order.amountIn, U256::from(2_880_000u32));
        let pins = types::config::AcrossPins {
            periphery: swapic::evm(order.steps[0].target),
            swap_proxy: types::EvmAddress::ZERO,
            spoke_pool: "0x09aea4b2242abC8bb4BB78D537A67a245A7bEC64".parse().unwrap(),
            multicall_handler: types::EvmAddress::ZERO,
        };
        let read = types::swapic::across::read_across_step(step, &pins).unwrap();
        let was = types::swapic::across::read_across_step(&order.steps[0], &pins).unwrap();
        // floor(2,930,727 x 2.88 / 3)
        assert_eq!(read.output_amount, U256::from(2_813_497u32));
        assert_eq!(
            types::swapic::across::AcrossDeposit {
                output_amount: was.output_amount,
                min_expected: was.min_expected,
                ..read
            },
            was
        );
        assert_eq!(fitter.0.borrow().as_ref(), Some(&fitted.order));
    });
    // a plan that swaps before its deposit cannot be fitted: it waits for the watcher's order
    let mut both = order.clone();
    both.steps.insert(0, swap_step(route(), 0));
    let both = PinnedOrder {
        order: swapic::encode_order(&both),
        ..across_plan(1_000)
    };
    assert!(matches!(
        decide(&both, planned, amount(3_000_000), 100),
        Err(Fit::Wait(_))
    ));
}
