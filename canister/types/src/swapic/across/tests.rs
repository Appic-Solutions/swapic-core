//! Held to T0, the live Across test of 2026-11-08 (`samples/t0.json`): 3 USDC from Base, a
//! relayer's fill on Arbitrum into the Runner, delivered as ARB.

use super::*;
use crate::swapic::Action;
use serde_json::Value;

fn t0() -> Value {
    serde_json::from_str(include_str!("samples/t0.json")).unwrap()
}

fn a(v: &Value) -> EvmAddress {
    let text = v.as_str().unwrap();
    let bytes: [u8; 20] = hex::decode(text.trim_start_matches("0x"))
        .unwrap()
        .try_into()
        .unwrap();
    EvmAddress::new(bytes)
}

fn b(v: &Value) -> Vec<u8> {
    hex::decode(v.as_str().unwrap().trim_start_matches("0x")).unwrap()
}

fn n(v: &Value) -> U256 {
    v.as_str().unwrap().parse().unwrap()
}

fn pins(t: &Value) -> AcrossPins {
    AcrossPins {
        periphery: a(&t["periphery"]),
        swap_proxy: EvmAddress::new([0x1d; 20]),
        spoke_pool: a(&t["spoke_pool"]),
        multicall_handler: a(&t["recipient"]),
    }
}

fn recipe(t: &Value) -> Recipe {
    Recipe {
        target: addr(a(&t["recipe"]["target"])),
        data: b(&t["recipe"]["data"]).into(),
        scaler: addr(a(&t["recipe"]["scaler"])),
    }
}

fn deposit(t: &Value) -> AcrossDeposit {
    AcrossDeposit {
        periphery: a(&t["periphery"]),
        spoke_pool: a(&t["spoke_pool"]),
        input_token: a(&t["input_token"]),
        output_token: a(&t["output_token"]),
        output_amount: n(&t["output_amount"]),
        min_expected: n(&t["min_expected"]),
        depositor: a(&t["depositor"]),
        recipient: a(&t["recipient"]),
        destination_chain_id: t["destination_chain_id"].as_u64().unwrap(),
        exclusive_relayer: a(&t["exclusive_relayer"]),
        quote_timestamp: t["quote_timestamp"].as_u64().unwrap() as u32,
        fill_deadline: t["fill_deadline"].as_u64().unwrap() as u32,
        exclusivity: t["exclusivity"].as_u64().unwrap() as u32,
        message: b(&t["message"]),
    }
}

#[test]
fn the_deposit_and_its_message_are_the_ones_t0_filled() {
    let t = t0();
    let d = deposit(&t);
    assert_eq!(d.calldata(), b(&t["periphery_data"]));
    let message = arrival_message(
        a(&t["recipient"]),
        a(&t["output_token"]),
        a(&t["runner"]),
        b(&t["envelope"]),
        recipe(&t),
    );
    assert_eq!(message, b(&t["message"]));
}

#[test]
fn t0s_step_reads_back_as_its_deposit() {
    let t = t0();
    let d = deposit(&t);
    assert_eq!(read_across_step(&d.step(), &pins(&t)), Ok(d));
}

#[test]
fn a_step_off_the_pins_or_off_the_identity_deposit_is_refused() {
    let t = t0();
    let d = deposit(&t);
    let p = pins(&t);
    // another target
    let mut step = d.step();
    step.target = addr(p.spoke_pool);
    assert_eq!(
        read_across_step(&step, &p),
        Err(StepError::NotCanonical("target"))
    );
    // another SpokePool, rebuilt honestly (AX4)
    let mut other = d.clone();
    other.spoke_pool = EvmAddress::new([0x77; 20]);
    assert_eq!(
        read_across_step(&other.step(), &p),
        Err(StepError::NotCanonical("spokePool"))
    );
    // a signed swapTokenAmount, the proportional flag off, a nonce
    for (byte, value) in [(259usize, 1u8), (355, 0), (419, 7)] {
        let mut step = d.step();
        let mut data = step.data.to_vec();
        data[byte] = value;
        step.data = data.into();
        assert_eq!(
            read_across_step(&step, &p),
            Err(StepError::NotCanonical("calldata")),
            "byte {byte}"
        );
    }
    // another write, token or least
    let mut step = d.step();
    step.writes[0].minus = U256::from(1u8);
    assert_eq!(
        read_across_step(&step, &p),
        Err(StepError::NotCanonical("writes"))
    );
    let mut step = d.step();
    step.token = addr(d.output_token);
    assert_eq!(
        read_across_step(&step, &p),
        Err(StepError::NotCanonical("token"))
    );
    let mut step = d.step();
    step.min -= U256::from(1u8);
    assert_eq!(
        read_across_step(&step, &p),
        Err(StepError::NotCanonical("min"))
    );
    // not swapAndBridge at all
    let mut step = d.step();
    step.data = vec![0xde, 0xad, 0xbe, 0xef].into();
    assert_eq!(read_across_step(&step, &p), Err(StepError::Unknown));
}

#[test]
fn t0s_message_reads_back_as_its_envelope_and_recipe() {
    let t = t0();
    let (envelope, got) = read_arrival(
        &b(&t["message"]),
        a(&t["recipient"]),
        a(&t["output_token"]),
        a(&t["runner"]),
    )
    .unwrap();
    assert_eq!(envelope.encode(), b(&t["envelope"]));
    assert_eq!(envelope.action, Action::Swap);
    assert_eq!(got, recipe(&t));
}

#[test]
fn a_message_naming_other_contracts_or_a_fallback_is_refused() {
    let t = t0();
    let message = b(&t["message"]);
    let (handler, stable, runner) = (a(&t["recipient"]), a(&t["output_token"]), a(&t["runner"]));
    let other = EvmAddress::new([0x77; 20]);
    for (h, s, r) in [
        (other, stable, runner),
        (handler, other, runner),
        (handler, stable, other),
    ] {
        assert_eq!(
            read_arrival(&message, h, s, r),
            Err(StepError::NotCanonical("message"))
        );
    }
    let mut instructions = Instructions::abi_decode(&message).unwrap();
    instructions.fallbackRecipient = addr(runner);
    assert_eq!(
        read_arrival(&instructions.abi_encode(), handler, stable, runner),
        Err(StepError::NotCanonical("message"))
    );
    assert_eq!(
        read_arrival(&[], handler, stable, runner),
        Err(StepError::NotCanonical("message"))
    );
}

#[test]
fn the_integrator_suffix_is_1dc0de_and_two_bytes() {
    assert_eq!(integrator_suffix(0x00ab), [0x1d, 0xc0, 0xde, 0x00, 0xab]);
    assert_eq!(integrator_suffix(0x12cd), [0x1d, 0xc0, 0xde, 0x12, 0xcd]);
}
