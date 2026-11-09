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
    let bytes: [u8; 20] = hex::decode(text.trim_start_matches("0x")).unwrap().try_into().unwrap();
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
    assert_eq!(read_across_step(&step, &p), Err(StepError::NotCanonical("target")));
    // another SpokePool, rebuilt honestly (AX4)
    let mut other = d.clone();
    other.spoke_pool = EvmAddress::new([0x77; 20]);
    assert_eq!(read_across_step(&other.step(), &p), Err(StepError::NotCanonical("spokePool")));
    // a signed swapTokenAmount, the proportional flag off, a nonce
    for (byte, value) in [(259usize, 1u8), (355, 0), (419, 7)] {
        let mut step = d.step();
        let mut data = step.data.to_vec();
        data[byte] = value;
        step.data = data.into();
        assert_eq!(read_across_step(&step, &p), Err(StepError::NotCanonical("calldata")), "byte {byte}");
    }
    // another write, token or least
    let mut step = d.step();
    step.writes[0].minus = U256::from(1u8);
    assert_eq!(read_across_step(&step, &p), Err(StepError::NotCanonical("writes")));
    let mut step = d.step();
    step.token = addr(d.output_token);
    assert_eq!(read_across_step(&step, &p), Err(StepError::NotCanonical("token")));
    let mut step = d.step();
    step.min -= U256::from(1u8);
    assert_eq!(read_across_step(&step, &p), Err(StepError::NotCanonical("min")));
    // not swapAndBridge at all
    let mut step = d.step();
    step.data = vec![0xde, 0xad, 0xbe, 0xef].into();
    assert_eq!(read_across_step(&step, &p), Err(StepError::Unknown));
}

#[test]
fn t0s_message_reads_back_as_its_envelope_and_recipe() {
    let t = t0();
    let (envelope, got) = read_arrival(&b(&t["message"]), a(&t["recipient"]), a(&t["output_token"]), a(&t["runner"])).unwrap();
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
    for (h, s, r) in [(other, stable, runner), (handler, other, runner), (handler, stable, other)] {
        assert_eq!(read_arrival(&message, h, s, r), Err(StepError::NotCanonical("message")));
    }
    let mut instructions = Instructions::abi_decode(&message).unwrap();
    instructions.fallbackRecipient = addr(runner);
    assert_eq!(
        read_arrival(&instructions.abi_encode(), handler, stable, runner),
        Err(StepError::NotCanonical("message"))
    );
    assert_eq!(read_arrival(&[], handler, stable, runner), Err(StepError::NotCanonical("message")));
}

#[test]
fn the_integrator_suffix_is_1dc0de_and_two_bytes() {
    assert_eq!(integrator_suffix(0x00ab), [0x1d, 0xc0, 0xde, 0x00, 0xab]);
    assert_eq!(integrator_suffix(0x12cd), [0x1d, 0xc0, 0xde, 0x12, 0xcd]);
}

/// T0's receipts as the nodes returned them: the Base source (`OrderStarted`, the periphery's
/// pull and swap, `FundsDeposited`) and the Arbitrum fill (`FilledRelay`, the handler into the
/// Runner).
fn receipts() -> Value {
    serde_json::from_str(include_str!("samples/t0_receipts.json")).unwrap()
}

fn log_parts(log: &Value) -> (Vec<[u8; 32]>, Vec<u8>) {
    let topics = log["topics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|topic| b(topic).try_into().unwrap())
        .collect();
    (topics, b(&log["data"]))
}

fn logs_of(receipt: &Value, topic: [u8; 32]) -> Vec<(Vec<[u8; 32]>, Vec<u8>)> {
    receipt["logs"]
        .as_array()
        .unwrap()
        .iter()
        .map(log_parts)
        .filter(|(topics, _)| topics.first() == Some(&topic))
        .collect()
}

fn word(address: EvmAddress) -> [u8; 32] {
    addr(address).into_word().0
}

/// T0's deposit as Base's SpokePool logged it.
fn t0_deposited() -> Deposited {
    let r = receipts();
    let (topics, data) = logs_of(&r["source"], topics::funds_deposited()).remove(0);
    read_funds_deposited(&topics, &data, ChainId::BASE).unwrap()
}

#[test]
fn t0s_relay_hashes_to_what_arbitrums_spoke_pool_answered() {
    let deposited = t0_deposited();
    assert_eq!(deposited.destination, 42_161);
    let relay = &deposited.relay;
    assert_eq!(relay.input_amount, TokenAmount::from(3_000_000u32));
    assert_eq!(relay.output_amount, TokenAmount::from(2_930_727u32));
    assert_eq!(relay.deposit_id, TokenAmount::from(6_434_219u32));
    assert_eq!(relay.fill_deadline, 1_794_161_828);
    // the log's exclusivity is the instant it ends, not the offset the step signs
    assert_eq!(relay.exclusivity_deadline, 1_794_160_691);
    assert_eq!(relay.message, b(&t0()["message"]));
    // `getV3RelayHash` read on Arbitrum after the fill (p0/t0/verify.json)
    let answered: [u8; 32] =
        hex::decode("d947afdcf5afc7f7270b7a9090f5211e29cbcbbac253643ca2ad06fe7e8ce202")
            .unwrap()
            .try_into()
            .unwrap();
    assert_eq!(relay.hash(ChainId::new(42_161)), answered);
    // on another destination it is another relay
    assert_ne!(relay.hash(ChainId::BASE), answered);
}

#[test]
fn t0s_fill_repeats_its_deposit_and_names_its_relayer() {
    let r = receipts();
    let (topics, data) = logs_of(&r["fill"], topics::filled_relay()).remove(0);
    let filled = read_filled_relay(&topics, &data).unwrap();
    assert_eq!(filled.key, t0_deposited().relay.key());
    assert_eq!(filled.fill_type, FillType::FastFill);
    assert_eq!(filled.relayer, word(a(&t0()["exclusive_relayer"])));
    assert_eq!(filled.updated_recipient, word(a(&t0()["recipient"])));
    assert_eq!(filled.updated_output_amount, U256::from(2_930_727u32));
    assert_eq!(filled.updated_message_hash, message_hash(&b(&t0()["message"])));
}

#[test]
fn t0s_periphery_logged_the_identity_swap() {
    let r = receipts();
    let (topics, data) = logs_of(&r["source"], topics::swap_before_bridge()).remove(0);
    let swapped = read_swap_before_bridge(&topics, &data).unwrap();
    let usdc = a(&t0()["input_token"]);
    assert_eq!(swapped.swap_token, usdc);
    assert_eq!(swapped.input_token, usdc);
    assert_eq!(swapped.swap_token_amount, U256::from(3_000_000u32));
    assert_eq!(swapped.input_amount, U256::from(3_000_000u32));
}

#[test]
fn a_log_of_another_event_reads_as_none() {
    let r = receipts();
    let (topics, data) = logs_of(&r["source"], topics::funds_deposited()).remove(0);
    assert_eq!(read_filled_relay(&topics, &data), None);
    assert_eq!(read_swap_before_bridge(&topics, &data), None);
    assert_eq!(read_refund_root(&topics, &data), None);
}

#[test]
fn a_refund_leaf_and_a_claim_read_back() {
    use alloy_sol_types::SolEvent;
    let canister = EvmAddress::new([0x8f; 20]);
    let usdc = a(&t0()["input_token"]);
    let leaf = ExecutedRelayerRefundRoot {
        amountToReturn: U256::ZERO,
        chainId: U256::from(8_453u32),
        refundAmounts: vec![U256::from(5u8), U256::from(3_000_000u32)],
        rootBundleId: 7,
        leafId: 2,
        l2TokenAddress: addr(usdc),
        refundAddresses: vec![Address::repeat_byte(0x11), addr(canister)],
        deferredRefunds: false,
        caller: Address::repeat_byte(0x22),
    };
    let encoded = leaf.encode_log_data();
    let topics: Vec<[u8; 32]> = encoded.topics().iter().map(|topic| topic.0).collect();
    let read = read_refund_root(&topics, &encoded.data).unwrap();
    assert_eq!(read.chain_id, 8_453);
    assert_eq!(read.token, usdc);
    assert_eq!(read.refunds[1], (canister, TokenAmount::from(3_000_000u32)));
    assert!(!read.deferred);
    let claim = ClaimedRelayerRefund {
        l2TokenAddress: addr(usdc).into_word(),
        refundAddress: addr(canister).into_word(),
        amount: U256::from(9u8),
        caller: addr(canister),
    };
    let encoded = claim.encode_log_data();
    let topics: Vec<[u8; 32]> = encoded.topics().iter().map(|topic| topic.0).collect();
    let read = read_claimed_refund(&topics, &encoded.data).unwrap();
    assert_eq!(read.refund_address, word(canister));
    assert_eq!(read.caller, canister);
    assert_eq!(read.amount, TokenAmount::from(9u8));
}

#[test]
fn a_fitted_deposit_keeps_the_ratio_and_every_other_byte() {
    let d = deposit(&t0());
    // T0's M is 3 USDC and F 2.930727
    let lower = d.fitted(U256::from(2_900_000u32)).unwrap();
    assert_eq!(lower.min_expected, U256::from(2_900_000u32));
    assert_eq!(lower.output_amount, U256::from(2_833_036u32)); // floor(2930727 * 2.9 / 3)
    assert_eq!(AcrossDeposit { min_expected: d.min_expected, output_amount: d.output_amount, ..lower.clone() }, d);
    let same = d.fitted(d.min_expected).unwrap();
    assert_eq!(same, d);
    // the step reads back with its least the amount
    assert_eq!(lower.step().min, U256::from(2_900_000u32));
}

#[test]
fn an_orders_one_across_step_is_found() {
    let d = deposit(&t0());
    let other = Step {
        target: Address::repeat_byte(0x01),
        data: vec![0xaa; 8].into(),
        token: Address::ZERO,
        min: U256::ZERO,
        writes: Vec::new(),
    };
    assert_eq!(across_step_at(&[other.clone(), d.step()]), Some(1));
    assert_eq!(across_step_at(&[other.clone()]), None);
    assert_eq!(across_step_at(&[d.step(), d.step()]), None);
}

#[test]
fn a_fill_status_reads_from_its_word() {
    let mut word = [0u8; 32];
    assert_eq!(FillStatus::of(word), Some(FillStatus::Unfilled));
    word[31] = 2;
    assert_eq!(FillStatus::of(word), Some(FillStatus::Filled));
    word[31] = 3;
    assert_eq!(FillStatus::of(word), None);
    word[31] = 2;
    word[0] = 1;
    assert_eq!(FillStatus::of(word), None);
}
